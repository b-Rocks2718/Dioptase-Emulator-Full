#!/usr/bin/env python3
"""
Convert a standard MIDI file into a DSYN v1 synth command stream.

The converter intentionally has no third-party dependencies. It supports
standard MIDI format 0 and 1 files with PPQ timing, note on/off events, running
status, and tempo changes. Each configured hardware channel is monophonic; if
two notes overlap on the same mapped hardware channel, the later note cuts the
earlier one short.
"""

from __future__ import annotations

import argparse
import copy
import io
import json
import os
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
from dataclasses import dataclass
from fractions import Fraction
from typing import Any, BinaryIO, Dict, Iterable, List, Optional, Tuple


SYNTH_BASE = 0x7FBC000
SAMPLE_RATE_HZ = 25_000
SYNTH_CLOCK_HZ = 100_000_000
DSYN_MAGIC = b"DSYN"
DSYN_VERSION = 1
DSYN_HEADER_BYTES = 32
DSYN_NO_LOOP = 0xFFFFFFFF

SYNTH_CTRL_OFFSET = 0x000
SYNTH_MASTER_VOLUME_OFFSET = 0x008
SYNTH_CTRL_ENABLE = 1 << 0
SYNTH_CTRL_RESET_STATE = 1 << 1

CHANNEL_STRIDE = 0x20
CH_CTRL_OFFSET = 0x00
CH_TIMER_OFFSET = 0x04
CH_VOLUME_OFFSET = 0x08
CH_LENGTH_OFFSET = 0x0C
CH_PHASE_OFFSET = 0x10
CH_TRIGGER_OFFSET = 0x14
CH_LENGTH_ENABLE = 1 << 1
CH_TRIGGER_START = 1 << 0
SQUARE_DUTY_SHIFT = 4
NOISE_SHORT_MODE = 1 << 4
SYNTH_AUDIO_CH_ENABLE = 1 << 0
SYNTH_AUDIO_MIX_SCALE = 16
SYNTH_AUDIO_MASTER_VOLUME_MAX = 255
SYNTH_AUDIO_PAGE_BYTES = 0x1000
AUDIO_I16_MIN = -32768
AUDIO_I16_MAX = 32767
DEFAULT_GUI_PREVIEW_SECONDS = 30

DEFAULT_TEMPO_US_PER_QUARTER = 500_000
MIDI_META_EVENT = 0xFF
MIDI_META_END_OF_TRACK = 0x2F
MIDI_META_SET_TEMPO = 0x51
MIDI_SYSEX_START = 0xF0
MIDI_SYSEX_CONTINUE = 0xF7
MIDI_NOTE_OFF = 0x80
MIDI_NOTE_ON = 0x90
MIDI_PROGRAM_CHANGE = 0xC0
MIDI_CHANNEL_PRESSURE = 0xD0


@dataclass(frozen=True)
class ChannelSpec:
    name: str
    kind: str
    offset: int
    phase_steps: int


CHANNEL_SPECS: Dict[str, ChannelSpec] = {
    "square0": ChannelSpec("square0", "square", 0x020, 8),
    "square1": ChannelSpec("square1", "square", 0x040, 8),
    "square2": ChannelSpec("square2", "square", 0x060, 8),
    "square3": ChannelSpec("square3", "square", 0x080, 8),
    "triangle0": ChannelSpec("triangle0", "triangle", 0x0A0, 32),
    "triangle1": ChannelSpec("triangle1", "triangle", 0x0C0, 32),
    "noise0": ChannelSpec("noise0", "noise", 0x0E0, 0),
    "noise1": ChannelSpec("noise1", "noise", 0x100, 0),
}


@dataclass(frozen=True)
class MidiEvent:
    tick: int
    order: int
    kind: str
    channel: Optional[int] = None
    note: Optional[int] = None
    velocity: Optional[int] = None
    tempo_us_per_quarter: Optional[int] = None


@dataclass(frozen=True)
class MidiNote:
    channel: int
    note: int
    velocity: int
    start_sample: int
    end_sample: int


@dataclass(frozen=True)
class DsynWrite:
    sample: int
    order: int
    reg_offset: int
    value: int


@dataclass
class ConversionStats:
    midi_notes: int
    dsyn_writes: int
    notes_by_channel: Dict[str, int]
    end_sample: int


class MidiError(ValueError):
    pass


def default_config() -> Dict[str, Any]:
    return {
        "master_volume": 255,
        "reset_at_start": True,
        "enable_at_start": True,
        "stop_at_end": True,
        "open_note_samples": SAMPLE_RATE_HZ,
        "channels": {
            "square0": {
                "midi_channel": 1,
                "duty": 2,
                "volume": 220,
                "velocity_scale": 1.0,
                "transpose": 0,
                "length_enable": True,
            },
            "square1": {
                "midi_channel": 2,
                "duty": 2,
                "volume": 200,
                "velocity_scale": 1.0,
                "transpose": 0,
                "length_enable": True,
            },
            "square2": {
                "midi_channel": 3,
                "duty": 1,
                "volume": 180,
                "velocity_scale": 1.0,
                "transpose": 0,
                "length_enable": True,
            },
            "square3": {
                "midi_channel": 4,
                "duty": 0,
                "volume": 160,
                "velocity_scale": 1.0,
                "transpose": 0,
                "length_enable": True,
            },
            "triangle0": {
                "midi_channel": 5,
                "volume": 220,
                "velocity_scale": 1.0,
                "transpose": -12,
                "length_enable": True,
            },
            "triangle1": {
                "midi_channel": 6,
                "volume": 180,
                "velocity_scale": 1.0,
                "transpose": 0,
                "length_enable": True,
            },
            "noise0": {
                "midi_channel": 10,
                "volume": 180,
                "velocity_scale": 1.0,
                "length_enable": True,
                "timer": 1200,
                "short_mode": False,
                "fixed_length_samples": 1500,
                "note_timers": {
                    "35": 3500,
                    "36": 3200,
                    "38": 1600,
                    "40": 1400,
                    "42": 550,
                    "44": 500,
                    "46": 450,
                    "49": 900,
                    "51": 850,
                },
            },
            "noise1": {
                "midi_channel": None,
                "volume": 140,
                "velocity_scale": 1.0,
                "length_enable": True,
                "timer": 700,
                "short_mode": True,
                "fixed_length_samples": 900,
                "note_timers": {},
            },
        },
    }


def read_u16_be(data: bytes, offset: int) -> int:
    return struct.unpack_from(">H", data, offset)[0]


def read_u32_be(data: bytes, offset: int) -> int:
    return struct.unpack_from(">I", data, offset)[0]


def read_varlen(data: bytes, offset: int) -> Tuple[int, int]:
    value = 0
    for _ in range(4):
        if offset >= len(data):
            raise MidiError("MIDI variable-length value extends past end of data")
        byte = data[offset]
        offset += 1
        value = (value << 7) | (byte & 0x7F)
        if (byte & 0x80) == 0:
            return value, offset
    raise MidiError("MIDI variable-length value uses more than four bytes")


def parse_midi(data: bytes) -> Tuple[int, List[MidiEvent]]:
    if len(data) < 14 or data[:4] != b"MThd":
        raise MidiError("input is not a standard MIDI file: missing MThd header")
    header_len = read_u32_be(data, 4)
    if header_len < 6:
        raise MidiError("MIDI header is too short")
    if len(data) < 8 + header_len:
        raise MidiError("MIDI header extends past end of file")

    midi_format = read_u16_be(data, 8)
    track_count = read_u16_be(data, 10)
    division = read_u16_be(data, 12)
    if midi_format not in (0, 1):
        raise MidiError(f"unsupported MIDI format {midi_format}; expected format 0 or 1")
    if (division & 0x8000) != 0:
        raise MidiError("SMPTE-time MIDI files are not supported; expected PPQ timing")
    if division == 0:
        raise MidiError("MIDI ticks-per-quarter-note must be nonzero")

    offset = 8 + header_len
    events: List[MidiEvent] = []
    order_base = 0
    for track_index in range(track_count):
        if offset + 8 > len(data):
            raise MidiError(f"MIDI track {track_index} header extends past end of file")
        if data[offset : offset + 4] != b"MTrk":
            raise MidiError(f"MIDI track {track_index} is missing MTrk header")
        track_len = read_u32_be(data, offset + 4)
        offset += 8
        track = data[offset : offset + track_len]
        if len(track) != track_len:
            raise MidiError(f"MIDI track {track_index} extends past end of file")
        track_events = parse_track(track, order_base)
        events.extend(track_events)
        order_base += len(track_events)
        offset += track_len

    events.sort(key=lambda event: (event.tick, event.order))
    return division, events


def parse_track(track: bytes, order_base: int) -> List[MidiEvent]:
    events: List[MidiEvent] = []
    tick = 0
    offset = 0
    running_status: Optional[int] = None
    order = order_base

    while offset < len(track):
        delta, offset = read_varlen(track, offset)
        tick += delta
        if offset >= len(track):
            raise MidiError("MIDI event status byte is missing")

        first = track[offset]
        if first & 0x80:
            status = first
            offset += 1
            if status not in (MIDI_SYSEX_START, MIDI_SYSEX_CONTINUE, MIDI_META_EVENT):
                running_status = status
        else:
            if running_status is None:
                raise MidiError("MIDI running-status event appears before any status byte")
            status = running_status

        if status == MIDI_META_EVENT:
            running_status = None
            if offset >= len(track):
                raise MidiError("MIDI meta event is missing its type byte")
            meta_type = track[offset]
            offset += 1
            length, offset = read_varlen(track, offset)
            payload = track[offset : offset + length]
            if len(payload) != length:
                raise MidiError("MIDI meta event extends past end of track")
            offset += length
            if meta_type == MIDI_META_SET_TEMPO:
                if length != 3:
                    raise MidiError("MIDI tempo meta event must contain exactly three bytes")
                tempo = (payload[0] << 16) | (payload[1] << 8) | payload[2]
                events.append(MidiEvent(tick, order, "tempo", tempo_us_per_quarter=tempo))
                order += 1
            elif meta_type == MIDI_META_END_OF_TRACK:
                break
            continue

        if status in (MIDI_SYSEX_START, MIDI_SYSEX_CONTINUE):
            running_status = None
            length, offset = read_varlen(track, offset)
            offset += length
            if offset > len(track):
                raise MidiError("MIDI SysEx event extends past end of track")
            continue

        status_kind = status & 0xF0
        channel = status & 0x0F
        data_len = 1 if status_kind in (MIDI_PROGRAM_CHANGE, MIDI_CHANNEL_PRESSURE) else 2
        payload = track[offset : offset + data_len]
        if len(payload) != data_len:
            raise MidiError("MIDI channel event extends past end of track")
        offset += data_len

        if status_kind == MIDI_NOTE_ON:
            note = payload[0]
            velocity = payload[1]
            kind = "note_off" if velocity == 0 else "note_on"
            events.append(MidiEvent(tick, order, kind, channel, note, velocity))
            order += 1
        elif status_kind == MIDI_NOTE_OFF:
            note = payload[0]
            velocity = payload[1]
            events.append(MidiEvent(tick, order, "note_off", channel, note, velocity))
            order += 1

    return events


def round_fraction(value: Fraction) -> int:
    return int(value + Fraction(1, 2))


def build_tick_sample_map(ticks_per_quarter: int, events: List[MidiEvent]) -> Dict[int, int]:
    tempo = DEFAULT_TEMPO_US_PER_QUARTER
    previous_tick = 0
    sample_cursor = Fraction(0, 1)
    tick_sample: Dict[int, int] = {}

    events_by_tick: Dict[int, List[MidiEvent]] = {}
    for event in events:
        events_by_tick.setdefault(event.tick, []).append(event)

    for tick in sorted(events_by_tick):
        delta_ticks = tick - previous_tick
        if delta_ticks < 0:
            raise MidiError("MIDI events are not sorted by tick")
        sample_cursor += Fraction(
            delta_ticks * tempo * SAMPLE_RATE_HZ,
            ticks_per_quarter * 1_000_000,
        )
        tick_sample[tick] = round_fraction(sample_cursor)
        for event in events_by_tick[tick]:
            if event.kind == "tempo":
                assert event.tempo_us_per_quarter is not None
                tempo = event.tempo_us_per_quarter
        previous_tick = tick

    return tick_sample


def extract_notes(
    ticks_per_quarter: int,
    events: List[MidiEvent],
    open_note_samples: int,
) -> Tuple[List[MidiNote], int]:
    tick_sample = build_tick_sample_map(ticks_per_quarter, events)
    active: Dict[Tuple[int, int], List[Tuple[int, int]]] = {}
    notes: List[MidiNote] = []
    last_sample = 0

    for event in events:
        sample = tick_sample[event.tick]
        last_sample = max(last_sample, sample)
        if event.kind not in ("note_on", "note_off"):
            continue
        assert event.channel is not None
        assert event.note is not None
        key = (event.channel, event.note)
        if event.kind == "note_on":
            velocity = event.velocity if event.velocity is not None else 64
            active.setdefault(key, []).append((sample, velocity))
        else:
            starts = active.get(key)
            if not starts:
                continue
            start_sample, velocity = starts.pop(0)
            if sample > start_sample:
                notes.append(MidiNote(event.channel, event.note, velocity, start_sample, sample))

    for (channel, note), starts in active.items():
        for start_sample, velocity in starts:
            end_sample = max(last_sample, start_sample + open_note_samples)
            notes.append(MidiNote(channel, note, velocity, start_sample, end_sample))
            last_sample = max(last_sample, end_sample)

    notes.sort(key=lambda note: (note.start_sample, note.channel, note.note))
    return notes, last_sample


def load_config(path: Optional[str], maps: Iterable[str], sets: Iterable[str]) -> Dict[str, Any]:
    config = default_config()
    if path is not None:
        with open(path, "r", encoding="utf-8") as config_file:
            override = json.load(config_file)
        merge_config(config, override)
    for mapping in maps:
        apply_channel_mapping(config, mapping)
    for assignment in sets:
        apply_config_assignment(config, assignment)
    validate_config(config)
    return config


def merge_config(base: Dict[str, Any], override: Dict[str, Any]) -> None:
    for key, value in override.items():
        if isinstance(value, dict) and isinstance(base.get(key), dict):
            merge_config(base[key], value)
        else:
            base[key] = value


def parse_value(raw: str) -> Any:
    lowered = raw.lower()
    if lowered in ("true", "yes", "on"):
        return True
    if lowered in ("false", "no", "off"):
        return False
    if lowered in ("none", "null"):
        return None
    try:
        return int(raw, 0)
    except ValueError:
        pass
    try:
        return float(raw)
    except ValueError:
        return raw


def apply_channel_mapping(config: Dict[str, Any], mapping: str) -> None:
    if "=" not in mapping:
        raise ValueError(f"invalid --map value {mapping!r}; expected hardware=midi_channel")
    hardware, channel_text = mapping.split("=", 1)
    hardware = hardware.strip().lower()
    if hardware not in CHANNEL_SPECS:
        raise ValueError(f"unknown hardware channel {hardware!r}")
    channel_text = channel_text.strip()
    if channel_text.lower() in ("off", "none", "null"):
        config["channels"][hardware]["midi_channel"] = None
    else:
        config["channels"][hardware]["midi_channel"] = parse_value(channel_text)


def apply_config_assignment(config: Dict[str, Any], assignment: str) -> None:
    if "=" not in assignment:
        raise ValueError(f"invalid --set value {assignment!r}; expected key=value")
    path, raw_value = assignment.split("=", 1)
    keys = path.strip().split(".")
    value = parse_value(raw_value.strip())
    if keys[-1] == "midi_channel" and value is False:
        value = None
    if len(keys) == 2 and keys[0] in CHANNEL_SPECS:
        target = config["channels"][keys[0]]
        target[keys[1]] = value
        return
    target = config
    for key in keys[:-1]:
        if key not in target or not isinstance(target[key], dict):
            target[key] = {}
        target = target[key]
    target[keys[-1]] = value


def validate_config(config: Dict[str, Any]) -> None:
    validate_u8(config.get("master_volume"), "master_volume")
    validate_nonnegative_int(config.get("open_note_samples"), "open_note_samples")
    channels = config.get("channels")
    if not isinstance(channels, dict):
        raise ValueError("config.channels must be an object")
    for name, spec in CHANNEL_SPECS.items():
        if name not in channels:
            raise ValueError(f"missing config for hardware channel {name}")
        channel_config = channels[name]
        if not isinstance(channel_config, dict):
            raise ValueError(f"config.channels.{name} must be an object")
        midi_channel = channel_config.get("midi_channel")
        if midi_channel is not None:
            validate_int_range(midi_channel, f"{name}.midi_channel", 1, 16)
        validate_u8(channel_config.get("volume", 255), f"{name}.volume")
        validate_bool(channel_config.get("length_enable", True), f"{name}.length_enable")
        if "velocity_scale" in channel_config and not isinstance(
            channel_config["velocity_scale"], (int, float)
        ):
            raise ValueError(f"{name}.velocity_scale must be a number")
        if spec.kind == "square":
            validate_int_range(channel_config.get("duty", 2), f"{name}.duty", 0, 3)
        if spec.kind == "noise":
            validate_nonnegative_int(channel_config.get("timer", 1200), f"{name}.timer")
            validate_bool(channel_config.get("short_mode", False), f"{name}.short_mode")
            if channel_config.get("fixed_length_samples") is not None:
                validate_nonnegative_int(
                    channel_config.get("fixed_length_samples"),
                    f"{name}.fixed_length_samples",
                )
            note_timers = channel_config.get("note_timers", {})
            if not isinstance(note_timers, dict):
                raise ValueError(f"{name}.note_timers must be an object")
            for note, timer in note_timers.items():
                validate_int_range(int(note), f"{name}.note_timers key", 0, 127)
                validate_nonnegative_int(timer, f"{name}.note_timers[{note}]")


def validate_int_range(value: Any, name: str, minimum: int, maximum: int) -> None:
    if isinstance(value, bool) or not isinstance(value, int) or not (minimum <= value <= maximum):
        raise ValueError(f"{name} must be an integer in [{minimum}, {maximum}]")


def validate_nonnegative_int(value: Any, name: str) -> None:
    if not isinstance(value, int) or value < 0:
        raise ValueError(f"{name} must be a nonnegative integer")


def validate_u8(value: Any, name: str) -> None:
    validate_int_range(value, name, 0, 255)


def validate_bool(value: Any, name: str) -> None:
    if not isinstance(value, bool):
        raise ValueError(f"{name} must be true or false")


def midi_note_frequency(note: int) -> float:
    return 440.0 * (2.0 ** ((note - 69) / 12.0))


def pitch_timer(note: int, phase_steps: int) -> int:
    note = max(0, min(127, note))
    period = SYNTH_CLOCK_HZ / (phase_steps * midi_note_frequency(note))
    return max(0, int(period + 0.5) - 1)


def clamp_u8(value: float) -> int:
    return max(0, min(255, int(value + 0.5)))


def note_volume(note: MidiNote, channel_config: Dict[str, Any]) -> int:
    base_volume = channel_config.get("volume", 255)
    velocity_scale = float(channel_config.get("velocity_scale", 1.0))
    return clamp_u8(base_volume * (note.velocity / 127.0) * velocity_scale)


def channel_ctrl_value(spec: ChannelSpec, channel_config: Dict[str, Any]) -> int:
    ctrl = 0
    if channel_config.get("length_enable", True):
        ctrl |= CH_LENGTH_ENABLE
    if spec.kind == "square":
        ctrl |= int(channel_config.get("duty", 2)) << SQUARE_DUTY_SHIFT
    elif spec.kind == "noise" and channel_config.get("short_mode", False):
        ctrl |= NOISE_SHORT_MODE
    return ctrl


def noise_timer(note: MidiNote, channel_config: Dict[str, Any]) -> int:
    note_timers = channel_config.get("note_timers", {})
    if str(note.note) in note_timers:
        return int(note_timers[str(note.note)])
    return int(channel_config.get("timer", 1200))


def convert_midi_to_dsyn_writes(
    midi_data: bytes,
    config: Dict[str, Any],
) -> Tuple[List[DsynWrite], ConversionStats]:
    ticks_per_quarter, midi_events = parse_midi(midi_data)
    notes, last_midi_sample = extract_notes(
        ticks_per_quarter,
        midi_events,
        int(config.get("open_note_samples", SAMPLE_RATE_HZ)),
    )

    writes: List[DsynWrite] = []
    order = 0

    def add_write(sample: int, reg_offset: int, value: int) -> None:
        nonlocal order
        writes.append(DsynWrite(max(0, sample), order, reg_offset, value & 0xFFFFFFFF))
        order += 1

    if config.get("reset_at_start", True) or config.get("enable_at_start", True):
        ctrl = 0
        if config.get("enable_at_start", True):
            ctrl |= SYNTH_CTRL_ENABLE
        if config.get("reset_at_start", True):
            ctrl |= SYNTH_CTRL_RESET_STATE
        add_write(0, SYNTH_CTRL_OFFSET, ctrl)
    add_write(0, SYNTH_MASTER_VOLUME_OFFSET, int(config.get("master_volume", 255)))

    notes_by_channel: Dict[str, int] = {}
    end_sample = last_midi_sample
    channels = config["channels"]
    for name, spec in CHANNEL_SPECS.items():
        channel_config = channels[name]
        midi_channel = channel_config.get("midi_channel")
        if midi_channel is None:
            continue
        midi_channel_index = int(midi_channel) - 1
        assigned = [note for note in notes if note.channel == midi_channel_index]
        assigned.sort(key=lambda note: (note.start_sample, note.note))
        if not assigned:
            continue
        notes_by_channel[name] = len(assigned)
        for index, note in enumerate(assigned):
            next_start = (
                assigned[index + 1].start_sample if index + 1 < len(assigned) else None
            )
            duration = note_duration_samples(note, channel_config)
            if next_start is not None and next_start > note.start_sample:
                duration = min(duration, next_start - note.start_sample)
            if duration <= 0:
                continue
            emit_note_writes(add_write, spec, channel_config, note, duration)
            note_end = note.start_sample + duration
            end_sample = max(end_sample, note_end)
            if not channel_config.get("length_enable", True):
                add_write(note_end, spec.offset + CH_CTRL_OFFSET, 0)

    if config.get("stop_at_end", True):
        add_write(end_sample, SYNTH_CTRL_OFFSET, 0)

    writes.sort(key=lambda write: (write.sample, write.order))
    stats = ConversionStats(len(notes), len(writes), notes_by_channel, end_sample)
    return writes, stats


def note_duration_samples(note: MidiNote, channel_config: Dict[str, Any]) -> int:
    fixed_length = channel_config.get("fixed_length_samples")
    if fixed_length is not None:
        return int(fixed_length)
    return max(1, note.end_sample - note.start_sample)


def emit_note_writes(
    add_write: Any,
    spec: ChannelSpec,
    channel_config: Dict[str, Any],
    note: MidiNote,
    duration: int,
) -> None:
    base = spec.offset
    transpose = int(channel_config.get("transpose", 0))
    if spec.kind == "square":
        timer = pitch_timer(note.note + transpose, spec.phase_steps)
    elif spec.kind == "triangle":
        timer = pitch_timer(note.note + transpose, spec.phase_steps)
    else:
        timer = noise_timer(note, channel_config)

    add_write(note.start_sample, base + CH_TIMER_OFFSET, timer)
    add_write(note.start_sample, base + CH_VOLUME_OFFSET, note_volume(note, channel_config))
    if channel_config.get("length_enable", True):
        add_write(note.start_sample, base + CH_LENGTH_OFFSET, duration)
    add_write(note.start_sample, base + CH_CTRL_OFFSET, channel_ctrl_value(spec, channel_config))
    add_write(note.start_sample, base + CH_TRIGGER_OFFSET, CH_TRIGGER_START)


def write_dsyn(stream: BinaryIO, writes: List[DsynWrite], loop_event: int = DSYN_NO_LOOP) -> None:
    stream.write(
        struct.pack(
            "<4sIIIIIII",
            DSYN_MAGIC,
            DSYN_VERSION,
            DSYN_HEADER_BYTES,
            SAMPLE_RATE_HZ,
            len(writes),
            loop_event,
            0,
            0,
        )
    )
    previous_sample = 0
    for write in writes:
        if write.sample < previous_sample:
            raise ValueError("DSYN writes must be sorted by sample")
        delta_samples = write.sample - previous_sample
        stream.write(struct.pack("<III", delta_samples, write.reg_offset, write.value))
        previous_sample = write.sample


def read_dsyn(data: bytes) -> Tuple[Tuple[Any, ...], List[Tuple[int, int, int]]]:
    if len(data) < DSYN_HEADER_BYTES:
        raise ValueError("DSYN data is too short for its header")
    header = struct.unpack_from("<4sIIIIIII", data, 0)
    if header[0] != DSYN_MAGIC:
        raise ValueError("DSYN magic mismatch")
    event_count = header[4]
    offset = DSYN_HEADER_BYTES
    events = []
    for _ in range(event_count):
        if offset + 12 > len(data):
            raise ValueError("DSYN event table is truncated")
        events.append(struct.unpack_from("<III", data, offset))
        offset += 12
    return header, events


def midi_channel_note_counts(midi_data: bytes) -> List[int]:
    ticks_per_quarter, midi_events = parse_midi(midi_data)
    notes, _ = extract_notes(ticks_per_quarter, midi_events, SAMPLE_RATE_HZ)
    counts = [0 for _ in range(16)]
    for note in notes:
        if 0 <= note.channel < len(counts):
            counts[note.channel] += 1
    return counts


def clamp_i16(value: int) -> int:
    return max(AUDIO_I16_MIN, min(AUDIO_I16_MAX, value))


def div_trunc_toward_zero(numerator: int, denominator: int) -> int:
    if numerator >= 0:
        return numerator // denominator
    return -((-numerator) // denominator)


@dataclass
class PreviewToneChannel:
    ctrl: int = 0
    timer: int = 0
    volume: int = 0
    length_reload: int = 0
    length_counter: int = 0
    phase: int = 0
    tick_accum: int = 0

    def length_enabled(self) -> bool:
        return (self.ctrl & CH_LENGTH_ENABLE) != 0

    def running(self) -> bool:
        return (self.ctrl & SYNTH_AUDIO_CH_ENABLE) != 0 and (
            not self.length_enabled() or self.length_counter != 0
        )

    def trigger(self) -> None:
        self.ctrl |= SYNTH_AUDIO_CH_ENABLE
        self.length_counter = self.length_reload
        self.phase = 0
        self.tick_accum = 0

    def finish_sample(self, phase_steps: int) -> None:
        if not self.running():
            return
        self.advance_phase(phase_steps)
        if self.length_enabled() and self.length_counter > 0:
            self.length_counter -= 1

    def advance_phase(self, phase_steps: int) -> None:
        period = self.timer + 1
        self.tick_accum += SYNTH_CLOCK_HZ // SAMPLE_RATE_HZ
        steps = self.tick_accum // period
        self.tick_accum %= period
        self.phase = (self.phase + (steps % phase_steps)) % phase_steps

    def square_sample(self) -> int:
        if not self.running():
            return 0
        duty_index = (self.ctrl & 0x30) >> SQUARE_DUTY_SHIFT
        high_steps = (1, 2, 4, 6)[min(duty_index, 3)]
        volume = self.volume & SYNTH_AUDIO_MASTER_VOLUME_MAX
        return volume if self.phase < high_steps else -volume

    def triangle_sample(self) -> int:
        triangle_levels = (
            15,
            14,
            13,
            12,
            11,
            10,
            9,
            8,
            7,
            6,
            5,
            4,
            3,
            2,
            1,
            0,
            0,
            1,
            2,
            3,
            4,
            5,
            6,
            7,
            8,
            9,
            10,
            11,
            12,
            13,
            14,
            15,
        )
        if not self.running():
            return 0
        level = triangle_levels[self.phase]
        volume = self.volume & SYNTH_AUDIO_MASTER_VOLUME_MAX
        return div_trunc_toward_zero(((level * 2) - 15) * volume, 15)


@dataclass
class PreviewNoiseChannel:
    ctrl: int = 0
    timer: int = 0
    volume: int = 0
    length_reload: int = 0
    length_counter: int = 0
    lfsr: int = 1
    tick_accum: int = 0

    def length_enabled(self) -> bool:
        return (self.ctrl & CH_LENGTH_ENABLE) != 0

    def running(self) -> bool:
        return (self.ctrl & SYNTH_AUDIO_CH_ENABLE) != 0 and (
            not self.length_enabled() or self.length_counter != 0
        )

    def trigger(self) -> None:
        self.ctrl |= SYNTH_AUDIO_CH_ENABLE
        self.length_counter = self.length_reload
        if self.lfsr == 0:
            self.lfsr = 1
        self.lfsr &= 0x7FFF
        self.tick_accum = 0

    def sample(self) -> int:
        if not self.running():
            return 0
        volume = self.volume & SYNTH_AUDIO_MASTER_VOLUME_MAX
        return volume if (self.lfsr & 1) != 0 else -volume

    def finish_sample(self) -> None:
        if not self.running():
            return
        self.advance_lfsr()
        if self.length_enabled() and self.length_counter > 0:
            self.length_counter -= 1

    def advance_lfsr(self) -> None:
        period = self.timer + 1
        self.tick_accum += SYNTH_CLOCK_HZ // SAMPLE_RATE_HZ
        steps = self.tick_accum // period
        self.tick_accum %= period
        tap_bit = 6 if (self.ctrl & NOISE_SHORT_MODE) != 0 else 1
        for _ in range(steps):
            feedback = (self.lfsr ^ (self.lfsr >> tap_bit)) & 1
            self.lfsr = ((self.lfsr >> 1) | (feedback << 14)) & 0x7FFF
            if self.lfsr == 0:
                self.lfsr = 1


class PreviewSynth:
    def __init__(self) -> None:
        self.ctrl = 0
        self.master_volume = SYNTH_AUDIO_MASTER_VOLUME_MAX
        self.squares = [PreviewToneChannel() for _ in range(4)]
        self.triangles = [PreviewToneChannel() for _ in range(2)]
        self.noises = [PreviewNoiseChannel() for _ in range(2)]

    def reset(self) -> None:
        self.__init__()

    def enabled(self) -> bool:
        return (self.ctrl & SYNTH_CTRL_ENABLE) != 0

    def write_reg(self, reg_offset: int, value: int) -> None:
        value &= 0xFFFFFFFF
        if reg_offset == SYNTH_CTRL_OFFSET:
            enable = value & SYNTH_CTRL_ENABLE
            if (value & SYNTH_CTRL_RESET_STATE) != 0:
                self.reset()
            self.ctrl = enable
            return
        if reg_offset == SYNTH_MASTER_VOLUME_OFFSET:
            self.master_volume = value & SYNTH_AUDIO_MASTER_VOLUME_MAX
            return
        if self._write_tone_channel(reg_offset, value, 0x020, self.squares, 0x33, 8):
            return
        if self._write_tone_channel(reg_offset, value, 0x0A0, self.triangles, 0x03, 32):
            return
        self._write_noise_channel(reg_offset, value)

    def _write_tone_channel(
        self,
        reg_offset: int,
        value: int,
        base: int,
        channels: List[PreviewToneChannel],
        ctrl_mask: int,
        phase_steps: int,
    ) -> bool:
        decoded = decode_preview_channel_offset(reg_offset, base, len(channels))
        if decoded is None:
            return False
        index, offset = decoded
        channel = channels[index]
        if offset == CH_CTRL_OFFSET:
            channel.ctrl = value & ctrl_mask
        elif offset == CH_TIMER_OFFSET:
            channel.timer = value
        elif offset == CH_VOLUME_OFFSET:
            channel.volume = value & SYNTH_AUDIO_MASTER_VOLUME_MAX
        elif offset == CH_LENGTH_OFFSET:
            channel.length_reload = value
        elif offset == CH_PHASE_OFFSET:
            channel.phase = value % phase_steps
        elif offset == CH_TRIGGER_OFFSET and (value & CH_TRIGGER_START) != 0:
            channel.trigger()
        return True

    def _write_noise_channel(self, reg_offset: int, value: int) -> bool:
        decoded = decode_preview_channel_offset(reg_offset, 0x0E0, len(self.noises))
        if decoded is None:
            return False
        index, offset = decoded
        channel = self.noises[index]
        if offset == CH_CTRL_OFFSET:
            channel.ctrl = value & (SYNTH_AUDIO_CH_ENABLE | CH_LENGTH_ENABLE | NOISE_SHORT_MODE)
        elif offset == CH_TIMER_OFFSET:
            channel.timer = value
        elif offset == CH_VOLUME_OFFSET:
            channel.volume = value & SYNTH_AUDIO_MASTER_VOLUME_MAX
        elif offset == CH_LENGTH_OFFSET:
            channel.length_reload = value
        elif offset == CH_PHASE_OFFSET:
            channel.lfsr = value & 0x7FFF
            if channel.lfsr == 0:
                channel.lfsr = 1
        elif offset == CH_TRIGGER_OFFSET and (value & CH_TRIGGER_START) != 0:
            channel.trigger()
        return True

    def consume_sample(self) -> int:
        if not self.enabled():
            return 0

        mixed = 0
        for channel in self.squares:
            mixed += channel.square_sample()
        for channel in self.triangles:
            mixed += channel.triangle_sample()
        for channel in self.noises:
            mixed += channel.sample()

        for channel in self.squares:
            channel.finish_sample(8)
        for channel in self.triangles:
            channel.finish_sample(32)
        for channel in self.noises:
            channel.finish_sample()

        scaled = div_trunc_toward_zero(
            mixed * SYNTH_AUDIO_MIX_SCALE * self.master_volume,
            SYNTH_AUDIO_MASTER_VOLUME_MAX,
        )
        return clamp_i16(scaled)


def decode_preview_channel_offset(
    reg_offset: int,
    base: int,
    channel_count: int,
) -> Optional[Tuple[int, int]]:
    if reg_offset < base:
        return None
    relative = reg_offset - base
    total = channel_count * CHANNEL_STRIDE
    if relative >= total:
        return None
    return relative // CHANNEL_STRIDE, relative % CHANNEL_STRIDE


def render_synth_pcm(
    writes: List[DsynWrite],
    total_samples: int,
    max_samples: Optional[int] = None,
) -> List[int]:
    if not writes and total_samples <= 0:
        return []
    sorted_writes = sorted(writes, key=lambda write: (write.sample, write.order))
    render_samples = max(total_samples, sorted_writes[-1].sample if sorted_writes else 0)
    if max_samples is not None:
        render_samples = min(render_samples, max(0, max_samples))

    synth = PreviewSynth()
    pcm: List[int] = []
    cursor = 0

    for write in sorted_writes:
        if write.sample > render_samples:
            break
        while cursor < write.sample:
            pcm.append(synth.consume_sample())
            cursor += 1
        synth.write_reg(write.reg_offset, write.value)

    while cursor < render_samples:
        pcm.append(synth.consume_sample())
        cursor += 1

    return pcm


def write_preview_wav(
    stream: BinaryIO,
    writes: List[DsynWrite],
    total_samples: int,
    max_samples: Optional[int] = None,
) -> None:
    import wave

    pcm = render_synth_pcm(writes, total_samples, max_samples)
    with wave.open(stream, "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(SAMPLE_RATE_HZ)
        for sample in pcm:
            wav.writeframesraw(struct.pack("<h", sample))


def preview_wav_bytes(
    writes: List[DsynWrite],
    total_samples: int,
    max_samples: Optional[int] = None,
) -> bytes:
    output = io.BytesIO()
    write_preview_wav(output, writes, total_samples, max_samples)
    return output.getvalue()


def audio_player_command(path: str) -> Optional[List[str]]:
    if shutil.which("ffplay"):
        return ["ffplay", "-nodisp", "-autoexit", "-loglevel", "error", path]
    if shutil.which("aplay"):
        return ["aplay", path]
    if shutil.which("paplay"):
        return ["paplay", path]
    if shutil.which("afplay"):
        return ["afplay", path]
    return None


def parse_note_timer_text(text: str) -> Dict[str, int]:
    text = text.strip()
    if not text:
        return {}
    if text.startswith("{"):
        value = json.loads(text)
        if not isinstance(value, dict):
            raise ValueError("noise note_timers must be a JSON object")
        return {str(int(note)): int(timer) for note, timer in value.items()}

    timers: Dict[str, int] = {}
    for item in text.split(","):
        item = item.strip()
        if not item:
            continue
        separator = ":" if ":" in item else "="
        if separator not in item:
            raise ValueError(
                "noise note_timers entries must be note:timer or note=timer pairs"
            )
        note_text, timer_text = item.split(separator, 1)
        timers[str(int(note_text.strip()))] = int(timer_text.strip(), 0)
    return timers


def note_timer_text(note_timers: Dict[str, Any]) -> str:
    if not note_timers:
        return ""
    return ",".join(
        f"{int(note)}:{int(timer)}" for note, timer in sorted(note_timers.items(), key=lambda x: int(x[0]))
    )


def launch_gui(
    initial_midi: Optional[str] = None,
    initial_output: Optional[str] = None,
    initial_config_path: Optional[str] = None,
) -> int:
    try:
        import tkinter as tk
        from tkinter import filedialog, messagebox, ttk
    except ImportError as error:
        print(f"midi_to_dsyn: Tkinter is unavailable: {error}", file=sys.stderr)
        return 1

    class MidiToDsynGui:
        def __init__(self, root: Any) -> None:
            self.root = root
            self.root.title("Dioptase MIDI to DSYN")
            self.midi_data: Optional[bytes] = None
            self.channel_counts = [0 for _ in range(16)]
            self.preview_process: Optional[subprocess.Popen[Any]] = None
            self.preview_paths: List[str] = []
            self.channel_vars: Dict[str, Dict[str, Any]] = {}
            self.channel_combos: Dict[str, Any] = {}

            self.midi_path_var = tk.StringVar(value=initial_midi or "")
            self.output_path_var = tk.StringVar(value=initial_output or "")
            self.status_var = tk.StringVar(value="Select a MIDI file to inspect its channels.")
            self.master_volume_var = tk.IntVar(value=255)
            self.reset_at_start_var = tk.BooleanVar(value=True)
            self.enable_at_start_var = tk.BooleanVar(value=True)
            self.stop_at_end_var = tk.BooleanVar(value=True)
            self.open_note_samples_var = tk.IntVar(value=SAMPLE_RATE_HZ)
            self.preview_seconds_var = tk.StringVar(value=str(DEFAULT_GUI_PREVIEW_SECONDS))

            self.build_ui()
            self.apply_config_to_controls(default_config())
            if initial_config_path:
                self.load_config_path(initial_config_path)
            if initial_midi:
                self.load_midi_path(initial_midi)

        def build_ui(self) -> None:
            outer = ttk.Frame(self.root, padding=10)
            outer.grid(row=0, column=0, sticky="nsew")
            self.root.columnconfigure(0, weight=1)
            self.root.rowconfigure(0, weight=1)

            file_frame = ttk.LabelFrame(outer, text="Files", padding=8)
            file_frame.grid(row=0, column=0, sticky="ew")
            file_frame.columnconfigure(1, weight=1)

            ttk.Label(file_frame, text="MIDI").grid(row=0, column=0, sticky="w")
            ttk.Entry(file_frame, textvariable=self.midi_path_var).grid(
                row=0, column=1, sticky="ew", padx=6
            )
            ttk.Button(file_frame, text="Browse", command=self.browse_midi).grid(
                row=0, column=2
            )

            ttk.Label(file_frame, text="DSYN").grid(row=1, column=0, sticky="w")
            ttk.Entry(file_frame, textvariable=self.output_path_var).grid(
                row=1, column=1, sticky="ew", padx=6
            )
            ttk.Button(file_frame, text="Browse", command=self.browse_output).grid(
                row=1, column=2
            )

            global_frame = ttk.LabelFrame(outer, text="Global Settings", padding=8)
            global_frame.grid(row=1, column=0, sticky="ew", pady=(8, 0))
            for col in range(8):
                global_frame.columnconfigure(col, weight=0)
            ttk.Label(global_frame, text="Master volume").grid(row=0, column=0, sticky="w")
            tk.Spinbox(
                global_frame,
                from_=0,
                to=255,
                width=5,
                textvariable=self.master_volume_var,
            ).grid(row=0, column=1, sticky="w", padx=(4, 14))
            ttk.Label(global_frame, text="Open note samples").grid(row=0, column=2, sticky="w")
            tk.Spinbox(
                global_frame,
                from_=0,
                to=SAMPLE_RATE_HZ * 60,
                increment=100,
                width=8,
                textvariable=self.open_note_samples_var,
            ).grid(row=0, column=3, sticky="w", padx=(4, 14))
            ttk.Label(global_frame, text="Preview seconds").grid(row=0, column=4, sticky="w")
            ttk.Entry(global_frame, width=8, textvariable=self.preview_seconds_var).grid(
                row=0, column=5, sticky="w", padx=(4, 14)
            )
            ttk.Checkbutton(
                global_frame,
                text="Reset",
                variable=self.reset_at_start_var,
            ).grid(row=0, column=6, sticky="w")
            ttk.Checkbutton(
                global_frame,
                text="Enable",
                variable=self.enable_at_start_var,
            ).grid(row=0, column=7, sticky="w")
            ttk.Checkbutton(
                global_frame,
                text="Stop at end",
                variable=self.stop_at_end_var,
            ).grid(row=0, column=8, sticky="w")

            channel_frame = ttk.LabelFrame(outer, text="Hardware Channels", padding=8)
            channel_frame.grid(row=2, column=0, sticky="nsew", pady=(8, 0))
            outer.rowconfigure(2, weight=1)
            headers = (
                "Channel",
                "MIDI",
                "Volume",
                "Velocity",
                "Length",
                "Transpose",
                "Duty/Timer",
                "Short",
                "Fixed Len",
                "Note Timers",
            )
            for col, header in enumerate(headers):
                ttk.Label(channel_frame, text=header).grid(row=0, column=col, sticky="w", padx=3)

            for row, (name, spec) in enumerate(CHANNEL_SPECS.items(), start=1):
                self.add_channel_row(channel_frame, row, name, spec)

            action_frame = ttk.Frame(outer)
            action_frame.grid(row=3, column=0, sticky="ew", pady=(8, 0))
            ttk.Button(action_frame, text="Convert", command=self.convert).grid(
                row=0, column=0, padx=(0, 6)
            )
            ttk.Button(action_frame, text="Play Preview", command=self.play_preview).grid(
                row=0, column=1, padx=(0, 6)
            )
            ttk.Button(action_frame, text="Stop Preview", command=self.stop_preview).grid(
                row=0, column=2, padx=(0, 6)
            )
            ttk.Button(action_frame, text="Save Preview WAV", command=self.save_preview_wav).grid(
                row=0, column=3, padx=(0, 6)
            )
            ttk.Button(action_frame, text="Load Config", command=self.browse_load_config).grid(
                row=0, column=4, padx=(0, 6)
            )
            ttk.Button(action_frame, text="Save Config", command=self.browse_save_config).grid(
                row=0, column=5
            )

            ttk.Label(outer, textvariable=self.status_var).grid(
                row=4, column=0, sticky="ew", pady=(8, 0)
            )

        def add_channel_row(self, parent: Any, row: int, name: str, spec: ChannelSpec) -> None:
            vars_for_channel: Dict[str, Any] = {
                "midi_channel": tk.StringVar(value="off"),
                "volume": tk.IntVar(value=255),
                "velocity_scale": tk.StringVar(value="1.0"),
                "length_enable": tk.BooleanVar(value=True),
                "transpose": tk.IntVar(value=0),
                "duty": tk.StringVar(value="2: 50%"),
                "timer": tk.IntVar(value=1200),
                "short_mode": tk.BooleanVar(value=False),
                "fixed_length_samples": tk.StringVar(value=""),
                "note_timers": tk.StringVar(value=""),
            }
            self.channel_vars[name] = vars_for_channel

            ttk.Label(parent, text=name).grid(row=row, column=0, sticky="w", padx=3)
            combo = ttk.Combobox(
                parent,
                textvariable=vars_for_channel["midi_channel"],
                width=16,
                state="readonly",
            )
            combo.grid(row=row, column=1, sticky="w", padx=3)
            self.channel_combos[name] = combo

            tk.Spinbox(
                parent,
                from_=0,
                to=255,
                width=5,
                textvariable=vars_for_channel["volume"],
            ).grid(row=row, column=2, sticky="w", padx=3)
            ttk.Entry(parent, width=7, textvariable=vars_for_channel["velocity_scale"]).grid(
                row=row, column=3, sticky="w", padx=3
            )
            ttk.Checkbutton(parent, variable=vars_for_channel["length_enable"]).grid(
                row=row, column=4, sticky="w", padx=3
            )

            if spec.kind in ("square", "triangle"):
                tk.Spinbox(
                    parent,
                    from_=-48,
                    to=48,
                    width=5,
                    textvariable=vars_for_channel["transpose"],
                ).grid(row=row, column=5, sticky="w", padx=3)
            else:
                ttk.Label(parent, text="-").grid(row=row, column=5, sticky="w", padx=3)

            if spec.kind == "square":
                ttk.Combobox(
                    parent,
                    textvariable=vars_for_channel["duty"],
                    width=10,
                    state="readonly",
                    values=("0: 12.5%", "1: 25%", "2: 50%", "3: 75%"),
                ).grid(row=row, column=6, sticky="w", padx=3)
            elif spec.kind == "noise":
                tk.Spinbox(
                    parent,
                    from_=0,
                    to=200000,
                    increment=50,
                    width=8,
                    textvariable=vars_for_channel["timer"],
                ).grid(row=row, column=6, sticky="w", padx=3)
            else:
                ttk.Label(parent, text="-").grid(row=row, column=6, sticky="w", padx=3)

            if spec.kind == "noise":
                ttk.Checkbutton(parent, variable=vars_for_channel["short_mode"]).grid(
                    row=row, column=7, sticky="w", padx=3
                )
                ttk.Entry(
                    parent,
                    width=8,
                    textvariable=vars_for_channel["fixed_length_samples"],
                ).grid(row=row, column=8, sticky="w", padx=3)
                ttk.Entry(parent, width=22, textvariable=vars_for_channel["note_timers"]).grid(
                    row=row, column=9, sticky="ew", padx=3
                )
            else:
                ttk.Label(parent, text="-").grid(row=row, column=7, sticky="w", padx=3)
                ttk.Label(parent, text="-").grid(row=row, column=8, sticky="w", padx=3)
                ttk.Label(parent, text="-").grid(row=row, column=9, sticky="w", padx=3)

        def browse_midi(self) -> None:
            path = filedialog.askopenfilename(
                title="Select MIDI file",
                filetypes=(("MIDI files", "*.mid *.midi"), ("All files", "*.*")),
            )
            if path:
                self.midi_path_var.set(path)
                self.load_midi_path(path)
                if not self.output_path_var.get():
                    self.output_path_var.set(os.path.splitext(path)[0] + ".dsyn")

        def browse_output(self) -> None:
            path = filedialog.asksaveasfilename(
                title="Save DSYN file",
                defaultextension=".dsyn",
                filetypes=(("DSYN files", "*.dsyn"), ("All files", "*.*")),
            )
            if path:
                self.output_path_var.set(path)

        def browse_load_config(self) -> None:
            path = filedialog.askopenfilename(
                title="Load synth config",
                filetypes=(("JSON files", "*.json"), ("All files", "*.*")),
            )
            if path:
                self.load_config_path(path)

        def browse_save_config(self) -> None:
            path = filedialog.asksaveasfilename(
                title="Save synth config",
                defaultextension=".json",
                filetypes=(("JSON files", "*.json"), ("All files", "*.*")),
            )
            if not path:
                return
            try:
                config = self.gather_config()
                with open(path, "w", encoding="utf-8") as config_file:
                    json.dump(config, config_file, indent=2, sort_keys=True)
                    config_file.write("\n")
                self.status_var.set(f"Saved config to {path}")
            except Exception as error:
                messagebox.showerror("Config Error", str(error))

        def load_config_path(self, path: str) -> None:
            try:
                config = load_config(path, [], [])
                self.apply_config_to_controls(config)
                self.status_var.set(f"Loaded config from {path}")
            except Exception as error:
                messagebox.showerror("Config Error", str(error))

        def load_midi_path(self, path: str) -> None:
            try:
                with open(path, "rb") as midi_file:
                    self.midi_data = midi_file.read()
                self.channel_counts = midi_channel_note_counts(self.midi_data)
                self.refresh_channel_choices()
                used = [
                    f"{index + 1}:{count}"
                    for index, count in enumerate(self.channel_counts)
                    if count
                ]
                channel_summary = ", ".join(used) if used else "none"
                self.status_var.set(f"Loaded MIDI channels: {channel_summary}")
            except Exception as error:
                self.midi_data = None
                messagebox.showerror("MIDI Error", str(error))

        def refresh_channel_choices(self) -> None:
            for name in CHANNEL_SPECS:
                combo = self.channel_combos[name]
                current = self.parse_midi_channel_label(combo.get())
                values = self.midi_channel_labels(current)
                combo.configure(values=values)
                combo.set(self.midi_channel_label(current))

        def midi_channel_labels(self, include_channel: Optional[int]) -> List[str]:
            channels = {
                index + 1 for index, count in enumerate(self.channel_counts) if count > 0
            }
            if include_channel is not None:
                channels.add(include_channel)
            if not channels and self.midi_data is None:
                channels.update(range(1, 17))
            return ["off"] + [self.midi_channel_label(channel) for channel in sorted(channels)]

        def midi_channel_label(self, channel: Optional[int]) -> str:
            if channel is None:
                return "off"
            count = self.channel_counts[channel - 1] if 1 <= channel <= 16 else 0
            return f"{channel} ({count} notes)"

        def parse_midi_channel_label(self, label: str) -> Optional[int]:
            if not label or label.lower().startswith("off"):
                return None
            return int(label.split()[0])

        def apply_config_to_controls(self, config: Dict[str, Any]) -> None:
            self.master_volume_var.set(int(config.get("master_volume", 255)))
            self.reset_at_start_var.set(bool(config.get("reset_at_start", True)))
            self.enable_at_start_var.set(bool(config.get("enable_at_start", True)))
            self.stop_at_end_var.set(bool(config.get("stop_at_end", True)))
            self.open_note_samples_var.set(int(config.get("open_note_samples", SAMPLE_RATE_HZ)))

            for name, spec in CHANNEL_SPECS.items():
                channel_config = config["channels"][name]
                vars_for_channel = self.channel_vars[name]
                midi_channel = channel_config.get("midi_channel")
                vars_for_channel["midi_channel"].set(self.midi_channel_label(midi_channel))
                vars_for_channel["volume"].set(int(channel_config.get("volume", 255)))
                vars_for_channel["velocity_scale"].set(
                    str(channel_config.get("velocity_scale", 1.0))
                )
                vars_for_channel["length_enable"].set(
                    bool(channel_config.get("length_enable", True))
                )
                if spec.kind in ("square", "triangle"):
                    vars_for_channel["transpose"].set(int(channel_config.get("transpose", 0)))
                if spec.kind == "square":
                    duty = int(channel_config.get("duty", 2))
                    duty_labels = ("0: 12.5%", "1: 25%", "2: 50%", "3: 75%")
                    vars_for_channel["duty"].set(duty_labels[duty])
                if spec.kind == "noise":
                    vars_for_channel["timer"].set(int(channel_config.get("timer", 1200)))
                    vars_for_channel["short_mode"].set(
                        bool(channel_config.get("short_mode", False))
                    )
                    fixed = channel_config.get("fixed_length_samples")
                    vars_for_channel["fixed_length_samples"].set(
                        "" if fixed is None else str(fixed)
                    )
                    vars_for_channel["note_timers"].set(
                        note_timer_text(channel_config.get("note_timers", {}))
                    )
            self.refresh_channel_choices()

        def gather_config(self) -> Dict[str, Any]:
            config = default_config()
            config["master_volume"] = int(self.master_volume_var.get())
            config["reset_at_start"] = bool(self.reset_at_start_var.get())
            config["enable_at_start"] = bool(self.enable_at_start_var.get())
            config["stop_at_end"] = bool(self.stop_at_end_var.get())
            config["open_note_samples"] = int(self.open_note_samples_var.get())

            for name, spec in CHANNEL_SPECS.items():
                vars_for_channel = self.channel_vars[name]
                channel_config = config["channels"][name]
                channel_config["midi_channel"] = self.parse_midi_channel_label(
                    vars_for_channel["midi_channel"].get()
                )
                channel_config["volume"] = int(vars_for_channel["volume"].get())
                channel_config["velocity_scale"] = float(
                    vars_for_channel["velocity_scale"].get()
                )
                channel_config["length_enable"] = bool(
                    vars_for_channel["length_enable"].get()
                )
                if spec.kind in ("square", "triangle"):
                    channel_config["transpose"] = int(vars_for_channel["transpose"].get())
                if spec.kind == "square":
                    channel_config["duty"] = int(
                        vars_for_channel["duty"].get().split(":", 1)[0]
                    )
                if spec.kind == "noise":
                    channel_config["timer"] = int(vars_for_channel["timer"].get())
                    channel_config["short_mode"] = bool(
                        vars_for_channel["short_mode"].get()
                    )
                    fixed = vars_for_channel["fixed_length_samples"].get().strip()
                    channel_config["fixed_length_samples"] = (
                        None if fixed == "" else int(fixed, 0)
                    )
                    channel_config["note_timers"] = parse_note_timer_text(
                        vars_for_channel["note_timers"].get()
                    )
            validate_config(config)
            return config

        def current_conversion(self) -> Tuple[List[DsynWrite], ConversionStats]:
            midi_data = self.current_midi_data()
            config = self.gather_config()
            return convert_midi_to_dsyn_writes(midi_data, config)

        def current_midi_data(self) -> bytes:
            path = self.midi_path_var.get()
            if self.midi_data is None:
                if not path:
                    raise ValueError("select a MIDI file first")
                with open(path, "rb") as midi_file:
                    self.midi_data = midi_file.read()
            elif path:
                with open(path, "rb") as midi_file:
                    self.midi_data = midi_file.read()
            return self.midi_data

        def preview_max_samples(self) -> Optional[int]:
            raw = self.preview_seconds_var.get().strip()
            if raw == "" or raw == "0":
                return None
            seconds = float(raw)
            if seconds < 0:
                raise ValueError("preview seconds must be nonnegative")
            return int(seconds * SAMPLE_RATE_HZ)

        def convert(self) -> None:
            try:
                output_path = self.output_path_var.get()
                if not output_path:
                    self.browse_output()
                    output_path = self.output_path_var.get()
                if not output_path:
                    return
                writes, stats = self.current_conversion()
                with open(output_path, "wb") as output_file:
                    write_dsyn(output_file, writes)
                self.status_var.set(
                    f"Wrote {stats.dsyn_writes} events for {stats.midi_notes} notes to {output_path}"
                )
            except Exception as error:
                messagebox.showerror("Conversion Error", str(error))

        def play_preview(self) -> None:
            self.stop_preview()
            try:
                writes, stats = self.current_conversion()
                wav_data = preview_wav_bytes(
                    writes,
                    stats.end_sample,
                    self.preview_max_samples(),
                )
                fd, path = tempfile.mkstemp(prefix="dioptase-dsyn-preview-", suffix=".wav")
                with os.fdopen(fd, "wb") as wav_file:
                    wav_file.write(wav_data)
                self.preview_paths.append(path)
                command = audio_player_command(path)
                if command is None:
                    messagebox.showerror(
                        "Preview Error",
                        "No audio player found. Install ffplay, aplay, paplay, or afplay, "
                        "or use Save Preview WAV.",
                    )
                    return
                self.preview_process = subprocess.Popen(
                    command,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                )
                self.status_var.set("Playing preview")
            except Exception as error:
                messagebox.showerror("Preview Error", str(error))

        def stop_preview(self) -> None:
            if self.preview_process is not None and self.preview_process.poll() is None:
                self.preview_process.terminate()
            self.preview_process = None
            remaining = []
            for path in self.preview_paths:
                try:
                    os.unlink(path)
                except OSError:
                    remaining.append(path)
            self.preview_paths = remaining

        def save_preview_wav(self) -> None:
            path = filedialog.asksaveasfilename(
                title="Save preview WAV",
                defaultextension=".wav",
                filetypes=(("WAV files", "*.wav"), ("All files", "*.*")),
            )
            if not path:
                return
            try:
                writes, stats = self.current_conversion()
                with open(path, "wb") as wav_file:
                    write_preview_wav(wav_file, writes, stats.end_sample, self.preview_max_samples())
                self.status_var.set(f"Saved preview WAV to {path}")
            except Exception as error:
                messagebox.showerror("Preview Error", str(error))

    root = tk.Tk()
    app = MidiToDsynGui(root)
    root.protocol("WM_DELETE_WINDOW", lambda: (app.stop_preview(), root.destroy()))
    root.mainloop()
    return 0


def write_default_config(path: str) -> None:
    with open(path, "w", encoding="utf-8") as config_file:
        json.dump(default_config(), config_file, indent=2, sort_keys=True)
        config_file.write("\n")


def make_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Convert a MIDI file into a DSYN v1 synth command stream.",
    )
    parser.add_argument("midi", nargs="?", help="input .mid file")
    parser.add_argument("output", nargs="?", help="output .dsyn file")
    parser.add_argument(
        "--gui",
        action="store_true",
        help="open a Tkinter GUI for mapping channels, conversion, and audio preview",
    )
    parser.add_argument("--config", help="JSON channel/config mapping file")
    parser.add_argument(
        "--write-default-config",
        metavar="PATH",
        help="write an editable default JSON config and exit",
    )
    parser.add_argument(
        "--map",
        action="append",
        default=[],
        metavar="HARDWARE=MIDI_CHANNEL",
        help="override a hardware channel mapping, e.g. square0=1 or noise1=off",
    )
    parser.add_argument(
        "--set",
        action="append",
        default=[],
        metavar="KEY=VALUE",
        help="override a config value, e.g. square0.duty=1 or master_volume=200",
    )
    parser.add_argument(
        "--dump-config",
        action="store_true",
        help="print the final merged config to stdout before conversion",
    )
    parser.add_argument("--self-test", action="store_true", help="run built-in tests and exit")
    return parser


def main(argv: Optional[List[str]] = None) -> int:
    parser = make_arg_parser()
    args = parser.parse_args(argv)
    if args.self_test:
        return run_self_tests()
    if args.write_default_config:
        write_default_config(args.write_default_config)
        return 0
    if args.gui:
        return launch_gui(args.midi, args.output, args.config)

    try:
        config = load_config(args.config, args.map, args.set)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        parser.error(str(error))

    if args.dump_config:
        json.dump(config, sys.stdout, indent=2, sort_keys=True)
        sys.stdout.write("\n")

    if args.midi is None or args.output is None:
        parser.error("midi and output paths are required unless --write-default-config is used")

    try:
        with open(args.midi, "rb") as midi_file:
            midi_data = midi_file.read()
        writes, stats = convert_midi_to_dsyn_writes(midi_data, config)
        with open(args.output, "wb") as output_file:
            write_dsyn(output_file, writes)
    except (OSError, MidiError, ValueError) as error:
        print(f"midi_to_dsyn: {error}", file=sys.stderr)
        return 1

    print(
        "midi_to_dsyn: wrote {} events for {} MIDI notes to {} ({} samples)".format(
            stats.dsyn_writes,
            stats.midi_notes,
            args.output,
            stats.end_sample,
        ),
        file=sys.stderr,
    )
    for name in sorted(stats.notes_by_channel):
        print(f"  {name}: {stats.notes_by_channel[name]} notes", file=sys.stderr)
    return 0


def varlen(value: int) -> bytes:
    parts = [value & 0x7F]
    value >>= 7
    while value:
        parts.append(0x80 | (value & 0x7F))
        value >>= 7
    return bytes(reversed(parts))


def make_test_midi() -> bytes:
    track = bytearray()
    track += varlen(0) + bytes([0xFF, 0x51, 0x03, 0x07, 0xA1, 0x20])
    track += varlen(0) + bytes([0x90, 69, 64])
    track += varlen(480) + bytes([0x80, 69, 0])
    track += varlen(0) + bytes([0xFF, 0x2F, 0x00])
    return (
        b"MThd"
        + struct.pack(">IHHH", 6, 0, 1, 480)
        + b"MTrk"
        + struct.pack(">I", len(track))
        + bytes(track)
    )


class MidiToDsynTests(unittest.TestCase):
    def test_parse_simple_midi(self) -> None:
        ticks_per_quarter, events = parse_midi(make_test_midi())
        self.assertEqual(ticks_per_quarter, 480)
        self.assertEqual([event.kind for event in events], ["tempo", "note_on", "note_off"])

    def test_extract_note_uses_tempo_for_sample_times(self) -> None:
        ticks_per_quarter, events = parse_midi(make_test_midi())
        notes, last_sample = extract_notes(ticks_per_quarter, events, SAMPLE_RATE_HZ)
        self.assertEqual(len(notes), 1)
        self.assertEqual(notes[0].start_sample, 0)
        self.assertEqual(notes[0].end_sample, SAMPLE_RATE_HZ // 2)
        self.assertEqual(last_sample, SAMPLE_RATE_HZ // 2)

    def test_convert_writes_dsyn_square_events(self) -> None:
        config = default_config()
        writes, stats = convert_midi_to_dsyn_writes(make_test_midi(), config)
        output = io.BytesIO()
        write_dsyn(output, writes)
        header, events = read_dsyn(output.getvalue())

        self.assertEqual(header[0], DSYN_MAGIC)
        self.assertEqual(header[3], SAMPLE_RATE_HZ)
        self.assertEqual(header[4], len(events))
        self.assertEqual(stats.notes_by_channel["square0"], 1)
        absolute = 0
        absolute_events = []
        for delta, reg_offset, value in events:
            absolute += delta
            absolute_events.append((absolute, reg_offset, value))
        self.assertIn((0, CHANNEL_SPECS["square0"].offset + CH_LENGTH_OFFSET, 12500), absolute_events)
        self.assertIn(
            (0, CHANNEL_SPECS["square0"].offset + CH_TRIGGER_OFFSET, CH_TRIGGER_START),
            absolute_events,
        )

    def test_midi_channel_note_counts(self) -> None:
        counts = midi_channel_note_counts(make_test_midi())
        self.assertEqual(counts[0], 1)
        self.assertEqual(sum(counts), 1)

    def test_preview_renderer_outputs_nonzero_wav(self) -> None:
        config = default_config()
        writes, stats = convert_midi_to_dsyn_writes(make_test_midi(), config)
        output = io.BytesIO()
        write_preview_wav(output, writes, stats.end_sample)
        wav_data = output.getvalue()
        self.assertTrue(wav_data.startswith(b"RIFF"))
        self.assertIn(b"WAVE", wav_data[:16])
        self.assertNotEqual(wav_data[-64:], b"\x00" * 64)

    def test_cli_overrides_channel_config(self) -> None:
        config = load_config(
            None,
            ["noise1=10", "square3=off"],
            ["square0.duty=1", "master_volume=200"],
        )
        self.assertEqual(config["channels"]["noise1"]["midi_channel"], 10)
        self.assertIsNone(config["channels"]["square3"]["midi_channel"])
        self.assertEqual(config["channels"]["square0"]["duty"], 1)
        self.assertEqual(config["master_volume"], 200)


def run_self_tests() -> int:
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(MidiToDsynTests)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    sys.exit(main())
