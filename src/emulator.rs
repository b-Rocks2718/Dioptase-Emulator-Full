use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead};
use std::path::Path;
use std::cmp;

use std::sync::{Arc, Mutex};
use std::thread;

use crate::memory::{Memory, PIT_START};
use crate::graphics::Graphics;

#[derive(Debug)]
pub struct RandomCache {
    table : HashMap<u32, u32>,
    size : usize,
    capacity : usize,
}

impl RandomCache {
  pub fn new(capacity : usize) -> RandomCache  {
    RandomCache {
      table : HashMap::new(),
      size : 0,
      capacity : capacity,
    }
  }

  pub fn read(&self, key : u32) -> Option<u32> {
    assert!(self.size <= self.capacity);
    self.table.get(&key).copied()
  }

  pub fn write(&mut self, key : u32, value : u32){
    if self.size < self.capacity {
      self.size += 1;
    } else {
      // remove an entry
      let evict = {
        let mut keys = self.table.keys();
        keys.next().cloned().expect("size was nonzero, this should work")
      };
      self.table.remove(&evict);
    }

    self.table.insert(key, value);
    assert!(self.size <= self.capacity);
  }

  pub fn clear(&mut self){
    self.size = 0;
    self.table.drain();
  }
}

pub struct Emulator {
  kmode : bool,
  regfile : [u32; 32],
  cregfile : [u32; 8],
  memory : Memory,
  tlb : RandomCache,
  pc : u32,
  flags : [bool; 4], // flags are: carry | zero | sign | overflow
  asleep : bool,
  halted : bool,
  timer : u32,
  count : u32,
  use_uart_rx: bool
}

fn read_lines<P>(filename: P) -> io::Result<io::Lines<io::BufReader<File>>>
where P: AsRef<Path>, {
    let file = File::open(filename)?;
    Ok(io::BufReader::new(file).lines())
}


impl Emulator {
  pub fn new(path : String, use_uart_rx: bool) -> Emulator {

    let mut instructions = HashMap::new();
    
    // read in binary file
    let lines = read_lines(path).expect("Couldn't open input file");
    // Consumes the iterator, returns an (Optional) String
    let mut pc : u32 = 0;
    for line in lines.map_while(Result::ok) {
      
      let bytes = line.as_bytes();
      if bytes.is_empty() {
        continue;
      }

      match bytes[0] {
        b'@' => {
          // Slice starting from index 1 (safe for ASCII)
          let addr_str = &line[1..];
          let addr = u32::from_str_radix(addr_str, 16).expect("Invalid address") * 4;
          pc = addr;
          continue;
        }
        _ => ()
      }

      // read one instruction
      let instruction = u32::from_str_radix(&line, 16).expect("Error parsing hex file");

      // write one instruction
      instructions.insert(pc, instruction as u8);
      instructions.insert(pc + 1, (instruction >> 8) as u8);
      instructions.insert(pc + 2, (instruction >> 16) as u8);
      instructions.insert(pc + 3, (instruction >> 24) as u8);

      pc += 4;
    }

    let mem: Memory = Memory::new(instructions, use_uart_rx);
    
    Emulator {
      kmode: true,
      regfile: [0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0],
      cregfile: [1, 0, 0, 0, 0, 0, 0, 0],
      memory: mem,
      tlb: RandomCache::new(8),
      pc: 0x400,
      flags: [false, false, false, false],
      asleep: false,
      halted: false,
      timer: 0,
      count: 0,
      use_uart_rx: use_uart_rx
    }
  }

  fn convert_mem_address(&self, addr : u32) -> Option<u32> {
    if self.kmode {
      if addr < 0x30000 {
        Some(addr)
      } else if let Some(result) = self.tlb.read(addr >> 12 | (self.cregfile[1] << 20)) {
        Some((result << 12) | (addr & 0xFFF))
      } else {
        // TLB_KMISS
        None
      }
    } else {
      if let Some(result) = self.tlb.read(addr >> 12 | (self.cregfile[1] << 20)) {
        Some((result << 12) | (addr & 0xFFF))
      } else {
        // TLB_UMISS
        None
      }
    }
  }

  fn is_addr_valid(&self, addr : u32) -> bool {
    if self.kmode {
      if addr < 0x30000 {
        true
      } else if let Some(_) = self.tlb.read(addr >> 12 | (self.cregfile[1] << 20)) {
        true
      } else {
        // TLB_KMISS
        false
      }
    } else {
      if let Some(_) = self.tlb.read(addr >> 12 | (self.cregfile[1] << 20)) {
        true
      } else {
        // TLB_UMISS
        false
      }
    }
  }

  fn save_state(&mut self){
    // save state as an interrupt happens

    // save pc and flags
    self.cregfile[4] = self.pc;
    self.cregfile[5] = 
      ((self.flags[3] as u32) << 3) |
      ((self.flags[2] as u32) << 2) |
      ((self.flags[1] as u32) << 1) |
      (self.flags[0] as u32);

    // disable interrupts
    self.cregfile[3] &= 0x7FFFFFFF;
  }

  fn raise_tlb_miss(&mut self, addr : u32) {
    // TLB_UMISS = 0x82
    // TLB_KMISS = 0x83

    // save address and pid that caused exception
    self.cregfile[7] = (addr >> 12) | (self.cregfile[1] << 20);

    self.save_state();

    if self.kmode {
      self.kmode = true;
      self.cregfile[0] += 1;
      self.pc = self.mem_read32(0x83 * 4).expect("shouldnt fail");
    } else {
      self.kmode = true;
      self.cregfile[0] += 1;
      self.pc = self.mem_read32(0x82 * 4).expect("shouldnt fail");
    }
  }

  fn mem_write8(&mut self, addr : u32, data : u8) -> bool {
    let addr = self.convert_mem_address(addr);

    if let Some(addr) = addr {
      self.memory.write(addr, data);
      true
    } else {
      false
    }
  }

  fn mem_write16(&mut self, addr : u32, data : u16) -> bool {
    if !self.is_addr_valid(addr) || !self.is_addr_valid(addr + 1) {
      return false;
    }

    self.mem_write8(addr, data as u8) &&
    self.mem_write8(addr + 1, (data >> 8) as u8)
  }

  fn mem_write32(&mut self, addr : u32, data : u32) -> bool {
    if !self.is_addr_valid(addr) || !self.is_addr_valid(addr + 1) ||
       !self.is_addr_valid(addr + 2) || !self.is_addr_valid(addr + 3) {
      return false;
    }

    self.mem_write16(addr, data as u16) &&
    self.mem_write16(addr + 2, (data >> 16) as u16)
  }

  fn mem_read8(&mut self, addr : u32) -> Option<u8> {
    let addr = self.convert_mem_address(addr);

    if let Some(addr) = addr {
      Some(self.memory.read(addr))
    } else {
      None
    }
  }

  fn mem_read16(&mut self, addr: u32) -> Option<u16> {
    if !self.is_addr_valid(addr) || !self.is_addr_valid(addr + 1) {
      return None;
    }
    self.mem_read8(addr).zip(self.mem_read8(addr + 1))
        .map(|(lo, hi)| (u16::from(hi) << 8) | u16::from(lo))
  }

  fn mem_read32(&mut self, addr: u32) -> Option<u32> {
    if !self.is_addr_valid(addr) || !self.is_addr_valid(addr + 1) ||
       !self.is_addr_valid(addr + 2) || !self.is_addr_valid(addr + 3) {
      return None;
    }

    self.mem_read16(addr).zip(self.mem_read16(addr + 2))
        .map(|(lo, hi)| (u32::from(hi) << 16) | u32::from(lo))
  }

  pub fn run(mut self, max_iters : u32, with_graphics : bool) -> Option<u32> {
    let mut graphics: Option<Graphics> = None;
    if with_graphics {
      graphics = Some(Graphics::new(
        self.memory.get_frame_buffer(), 
        self.memory.get_tile_map(), 
        self.memory.get_io_buffer(),
        self.memory.get_vscroll_register(),
        self.memory.get_hscroll_register(),
        self.memory.get_sprite_map(),
        self.memory.get_scale_register(),
      ));
    }

    // Return value and termination signal
    let ret: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
    let finished: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));

    // Runs emulator on thread because graphics must use main thread
    let handle = thread::spawn({
      let ret_clone = Arc::clone(&ret);
      let finished_clone = Arc::clone(&finished);
      move || {
        self.count = 0;
        while !self.halted {
          self.check_for_interrupts();
          self.handle_interrupts();

          if !self.asleep && ((self.count % cmp::max(u32::wrapping_add(self.cregfile[6], 1), 1)) == 0) {
            let instr = self.mem_read32(self.pc);

            if let Some(instr) = instr {
              self.execute(instr);
            } else {
              self.raise_tlb_miss(self.pc);
            }
          }
          if max_iters != 0 && self.count > max_iters {
            *ret_clone.lock().unwrap() = None;
            *finished_clone.lock().unwrap() = true;
            return;
          }
          self.count += 1;
        }

        // return the value in r3
        *ret_clone.lock().unwrap() = Some(self.regfile[3]);
        *finished_clone.lock().unwrap() = true;
      }
    });

    if with_graphics {
      graphics.unwrap().start(finished, false);
    }

    handle.join().unwrap();

    // return the value in r3
    return *ret.lock().unwrap();
  }

  fn check_for_interrupts(&mut self) {

    // check if io buf is nonempty
    let binding = self.memory.get_io_buffer();
    let io_buf = binding.read().unwrap();

    if !io_buf.is_empty() {
      if self.use_uart_rx {
        // cause a uart interrupt
        self.cregfile[2] |= 4;
      } else {
        // cause a keyboard interrupt
        self.cregfile[2] |= 2;
      }
    }

    // check for timer interrupt
    if self.timer == 0 {
      // check if timer was set
      let old_kmode = self.kmode;
      self.kmode = true;
      let v = self.mem_read32(PIT_START).unwrap();
      self.kmode = old_kmode;
      if v != 0 {
        // reset timer
        self.timer = v;

        // trigger timer interrupt
        self.cregfile[2] |= 1;
      }
    } else {
      self.timer -= 1;
    }
  }

  fn handle_interrupts(&mut self){
    if self.cregfile[3] >> 31 != 0 {
      // top bit activates/disables all interrupts
      let active_ints = self.cregfile[3] & self.cregfile[2];

      if active_ints == 0 {
        return;
      }

      // undo sleep
      if self.asleep {
        // move to next instruction
        self.pc += 1;
      }
      self.asleep = false;

      self.save_state();

      // enter kernel mode
      self.cregfile[0] += 1;
      self.kmode = true;

      // disable interrupts
      self.cregfile[3] &= 0x7FFFFFFF;

      if (active_ints >> 15) & 1 != 0 {
        self.pc = self.mem_read32(0xFF * 4).expect("this address shouldn't error");
      } else if (active_ints >> 14) & 1 != 0 {
        self.pc = self.mem_read32(0xFE * 4).expect("this address shouldn't error");
      } else if (active_ints >> 13) & 1 != 0 {
        self.pc = self.mem_read32(0xFD * 4).expect("this address shouldn't error");
      } else if (active_ints >> 12) & 1 != 0 {
        self.pc = self.mem_read32(0xFC * 4).expect("this address shouldn't error");
      } else if (active_ints >> 11) & 1 != 0 {
        self.pc = self.mem_read32(0xFB * 4).expect("this address shouldn't error");
      } else if (active_ints >> 10) & 1 != 0 {
        self.pc = self.mem_read32(0xFA * 4).expect("this address shouldn't error");
      } else if (active_ints >> 9) & 1 != 0 {
        self.pc = self.mem_read32(0xF9 * 4).expect("this address shouldn't error");
      } else if (active_ints >> 8) & 1 != 0{
        self.pc = self.mem_read32(0xF8 * 4).expect("this address shouldn't error");
      } else if (active_ints >> 7) & 1 != 0 {
        self.pc = self.mem_read32(0xF7 * 4).expect("this address shouldn't error");
      } else if (active_ints >> 6) & 1 != 0 {
        self.pc = self.mem_read32(0xF6 * 4).expect("this address shouldn't error");
      } else if (active_ints >> 5) & 1 != 0 {
        self.pc = self.mem_read32(0xF5 * 4).expect("this address shouldn't error");
      } else if (active_ints >> 4) & 1 != 0 {
        self.pc = self.mem_read32(0xF4 * 4).expect("this address shouldn't error");
      } else if (active_ints >> 3) & 1 != 0 {
        self.pc = self.mem_read32(0xF3 * 4).expect("this address shouldn't error");
      } else if (active_ints >> 2) & 1 != 0 {
        self.pc = self.mem_read32(0xF2 * 4).expect("this address shouldn't error");
      } else if (active_ints >> 1) & 1 != 0 {
        self.pc = self.mem_read32(0xF1 * 4).expect("this address shouldn't error");
      } else if active_ints & 1 != 0 {
        self.pc = self.mem_read32(0xF0 * 4).expect("this address shouldn't error");
      }
    }
  }

  fn raise_exc_instr(&mut self){
    // exec_instr

    self.save_state();

    self.kmode = true;
    self.cregfile[0] += 1;

    self.pc = self.mem_read32(0x80 * 4).expect("shouldn't fail");
    return;
  }

  fn execute(&mut self, instr : u32) {
    let opcode = instr >> 27; // opcode is top 5 bits of instruction

    match opcode {
      0 => self.alu_reg_op(instr),
      1 => self.alu_imm_op(instr),
      2 => self.load_upper_immediate(instr),
      3 => self.mem_absolute_w(instr),
      4 => self.mem_relative_w(instr),
      5 => self.mem_imm_w(instr),
      6 => self.mem_absolute_d(instr),
      7 => self.mem_relative_d(instr),
      8 => self.mem_imm_d(instr),
      9 => self.mem_absolute_b(instr),
      10 => self.mem_relative_b(instr),
      11 => self.mem_imm_b(instr),
      12 => self.branch_imm(instr),
      13 => self.branch_absolute(instr),
      14 => self.branch_relative(instr),
      15 => self.syscall(instr),
      31 => self.kernel_instr(instr),
      _ => self.raise_exc_instr(),
    }
  }

  fn alu_reg_op(&mut self, instr : u32) {
    // instruction format is
    // 00000aaaaabbbbbxxxxxxx?????ccccc
    // op (5 bits) | r_a (5 bits) | r_b (5 bits) | unused (7 bits) | op (5 bits) | r_c (5 bits)
    let r_a = (instr >> 22) & 0x1F;
    let r_b = (instr >> 17) & 0x1F;
    let op = (instr >> 5) & 0x1F;
    let r_c = instr & 0x1F;

    // retrieve arguments
    let r_b = self.regfile[r_b as usize];
    let r_c = self.regfile[r_c as usize];

    // carry flag is set differently for each instruction,
    // so its handled here. The other flags are all handled together
    let result = match op {
      0 => {
        self.flags[0] = false;
        r_b & r_c // and
      }, 
      1 => {
        self.flags[0] = false;
        !(r_b & r_c)  // nand
      },
      2 => {
        self.flags[0] = false;
        r_b | r_c // or
      },
      3 => {
        self.flags[0] = false;
        !(r_b | r_c) // nor
      },
      4 => {
        self.flags[0] = false;
        r_b ^ r_c // xor
      },
      5 => {
        self.flags[0] = false;
        !(r_b ^ r_c) // xnor
      },
      6 => {
        self.flags[0] = false;
        !r_c // not
      },
      7 => {
        // set carry flag
        self.flags[0] = r_b >> if r_c > 0 {32 - r_c} else {0} != 0;
        r_b << r_c // lsl
      },
      8 => {
        // set carry flag
        self.flags[0] = r_b & ((1 << r_c) - 1) != 0;
        r_b >> r_c // lsr
      },
      9 => {
        // set carry flag
        let carry = r_b & 1;
        let sign = r_b >> 31;
        self.flags[0] = carry != 0;
        (r_b >> r_c) | (0xFFFFFFFF * sign << if r_c > 0 {32 - r_c} else {0}) // asr
      },
      10 => {
        // set carry flag
        let carry = r_b >> if r_c > 0 {32 - r_c} else {0};
        self.flags[0] = carry != 0;
        (r_b << r_c) | carry // rotl
      },
      11 => {
        // set carry flag
        let carry = r_b & ((1 << r_c) - 1);
        self.flags[0] = carry != 0;
        (r_b >> r_c) | (carry << if r_c > 0 {32 - r_c} else {0}) // rotr
      },
      12 => {
        // set carry flag
        let carry = if r_c > 0 {r_b >> (32 - r_c)} else {0};
        let old_carry = u32::from(self.flags[0]);
        self.flags[0] = carry != 0;
        (r_b << r_c) | if r_c > 0 {old_carry << (r_c - 1)} else {0} // lslc
      },
      13 => {
        // set carry flag
        let carry = r_b & ((1 << r_c) - 1);
        let old_carry = u32::from(self.flags[0]);
        self.flags[0] = carry != 0;
        (r_b >> r_c) | (old_carry << if r_c > 0 {32 - r_c} else {0}) // lsrc
      },
      14 => {
        // add
        let result = u64::from(r_b) + u64::from(r_c);

        // set the carry flag
        self.flags[0] = result >> 32 != 0;

        result as u32
      },
      15 => {
        // addc
        let result = u64::from(r_c) + u64::from(r_b) + u64::from(self.flags[0]);

        // set the carry flag
        self.flags[0] = result >> 32 != 0;

        result as u32
      },
      16 => {
        // sub

        // two's complement
        let r_c = (1 + u64::from(!r_c)) as u32;
        let result = u64::from(r_c) + u64::from(r_b);

        // set the carry flag
        self.flags[0] = result >> 32 != 0;

        result as u32
      },
      17 => {
        // subb

        // two's complement
        let r_c = (1 + u64::from(
          !(u32::wrapping_add(
          u32::from(!self.flags[0]), r_c)))) as u32;
        let result = u64::from(r_c) + u64::from(r_b);

        // set the carry flag
        self.flags[0] = result >> 32 != 0;

        result as u32
      },
      18 => {
        // mul
        let result = u64::from(r_b) * u64::from(r_c);

        // set the carry flag
        self.flags[0] = result >> 32 != 0;

        result as u32
      },
      _ => {
        self.raise_exc_instr();
        return;
      }
    };

    // never update r0
    if r_a != 0 {
      self.regfile[r_a as usize] = result;
    }
    
    self.update_flags(result, r_b, r_c);

    self.pc += 4;

  }

  fn decode_alu_imm(&mut self, op : u32, imm : u32) -> Option<u32> {
    match op {
      0..=6 => {
        // Bitwise op
        Some((imm & 0xFF) << (8 * ((imm >> 8) & 3)))
      },
      7..=13 => {
        // Shift op
        Some(imm & 0x1F)
      },
      14..=18 => {
        // Arithmetic op
        Some(imm | (0xFFFFF000 * ((imm >> 11) & 1))) // sign extend
      },
      _ => {
        self.raise_exc_instr();
        return None
      }
    }
  }

  fn alu_imm_op(&mut self, instr : u32) {
    // instruction format is
    // 00000aaaaabbbbb?????iiiiiiiiiiii
    // op (5 bits) | r_a (5 bits) | r_b (5 bits) | op (5 bits) | imm (12 bits)
    let r_a = (instr >> 22) & 0x1F;
    let r_b = (instr >> 17) & 0x1F;
    let op = (instr >> 12) & 0x1F;
    let imm = instr & 0xFFF;

    let imm = self.decode_alu_imm(op, imm);

    if let Some(_) = imm {} 
    else {
      return;
    }

    let imm = imm.unwrap();

    // retrieve arguments
    let r_b = self.regfile[r_b as usize];

    // carry flag is set differently for each instruction,
    // so its handled here. The other flags are all handled together
    let result = match op {
      0 => {
        self.flags[0] = false;
        r_b & imm // and
      }, 
      1 => {
        self.flags[0] = false;
        !(r_b & imm)  // nand
      },
      2 => {
        self.flags[0] = false;
        r_b | imm // or
      },
      3 => {
        self.flags[0] = false;
        !(r_b | imm) // nor
      },
      4 => {
        self.flags[0] = false;
        r_b ^ imm // xor
      },
      5 => {
        self.flags[0] = false;
        !(r_b ^ imm) // xnor
      },
      6 => {
        self.flags[0] = false;
        !imm // not
      },
      7 => {
        // set carry flag
        self.flags[0] = r_b >> if imm > 0 {32 - imm} else {0} != 0;
        r_b << imm // lsl
      },
      8 => {
        // set carry flag
        self.flags[0] = r_b & ((1 << imm) - 1) != 0;
        r_b >> imm // lsr
      },
      9 => {
        // set carry flag
        let carry = r_b & 1;
        let sign = r_b >> 31;
        self.flags[0] = carry != 0;
        (r_b >> imm) | (0xFFFFFFFF * sign << if imm > 0 {32 - imm} else {0}) // asr
      },
      10 => {
        // set carry flag
        let carry = r_b >> if imm > 0 {32 - imm} else {0};
        self.flags[0] = carry != 0;
        (r_b << imm) | carry // rotl
      },
      11 => {
        // set carry flag
        let carry = r_b & ((1 << imm) - 1);
        self.flags[0] = carry != 0;
        (r_b >> imm) | (carry << if imm > 0 {32 - imm} else {0}) // rotr
      },
      12 => {
        // set carry flag
        let carry = r_b >> if imm > 0 {32 - imm} else {0};
        let old_carry = u32::from(self.flags[0]);
        self.flags[0] = carry != 0;
        (r_b << imm) | (old_carry << if imm > 0 {imm - 1} else {0}) // lslc
      },
      13 => {
        // set carry flag
        let carry = r_b & ((1 << imm) - 1);
        let old_carry = u32::from(self.flags[0]);
        self.flags[0] = carry != 0;
        (r_b >> imm) | (old_carry << if imm > 0 {32 - imm} else {0}) // lsrc
      },
      14 => {
        // add
        let result = u64::from(r_b) + u64::from(imm);

        // set the carry flag
        self.flags[0] = result >> 32 != 0;

        result as u32
      },
      15 => {
        // addc
        let result = u64::from(imm) + u64::from(r_b) + u64::from(self.flags[0]);

        // set the carry flag
        self.flags[0] = result >> 32 != 0;

        result as u32
      },
      16 => {
        // sub

        // two's complement
        let r_b = (1 + u64::from(!r_b)) as u32;
        let result = u64::from(imm) + u64::from(r_b);

        // set the carry flag
        self.flags[0] = result >> 32 != 0;

        result as u32
      },
      17 => {
        // subb

        // two's complement
        let r_b = (1 + u64::from(
          !(u32::wrapping_add(
          u32::from(!self.flags[0]), r_b)))) as u32;
        let result = u64::from(imm) + u64::from(r_b);

        // set the carry flag
        self.flags[0] = result >> 32 != 0;

        result as u32
      },
      18 => {
        // mul
        let result = u64::from(r_b) * u64::from(imm);

        // set the carry flag
        self.flags[0] = result >> 32 != 0;

        result as u32
      },
      _ => {
        self.raise_exc_instr();
        return;
      }
    };

    // never update r0
    if r_a != 0 {
      self.regfile[r_a as usize] = result;
    }
    
    self.update_flags(result, r_b, imm);

    self.pc += 4;
  }

  fn load_upper_immediate(&mut self, instr : u32){
    // store imm << 10 in r_a
    let r_a = (instr >> 22) & 0x1F;
    let imm = (instr & 0x03FFFFF) << 10;

    if r_a != 0 {
      self.regfile[r_a as usize] = imm;
    }

    self.pc += 4;
  }

  fn mem_absolute_w(&mut self, instr : u32){
    // instruction format is
    // 00011aaaaabbbbb?yyzziiiiiiiiiiii
    // op (5 bits) | r_a (5 bits) | r_b (5 bits) | op (1 bit) | y (2 bits) | z (2 bits) | imm (12 bits)

    let r_a = (instr >> 22) & 0x1F;
    let r_b = (instr >> 17) & 0x1F;
    let is_load = ((instr >> 16) & 1) != 0; // is this a load? else is store
    let y = (instr >> 14) & 3;
    let z = (instr >> 12) & 3;
    let imm = instr & 0xFFF;

    // sign extend imm
    let imm = imm | (0xFFFFF000 * ((imm >> 11) & 1));
    // shift imm
    let imm = imm << z;

    if y >= 4 {
      self.raise_exc_instr();
      return;
    };

    // get addr
    let r_b_out = self.regfile[r_b as usize];
    let addr = if y == 2 {r_b_out} else {u32::wrapping_add(r_b_out, imm)}; // check for postincrement

    if is_load {
      if r_a != 0 {
        self.regfile[r_a as usize] = 
        if let Some(data) = self.mem_read32(addr) {
          data
        } else{
          // TLB Miss
          self.raise_tlb_miss(addr);
          return;
        };
      }
    } else {
      // is a store
      let data = self.regfile[r_a as usize];
      if !self.mem_write32(addr, data) {
        // TLB Miss
        self.raise_tlb_miss(addr);
        return;
      }
    }

    if (y == 1 || y == 2) && (r_b != 0) {
      // pre or post increment
      self.regfile[r_b as usize] = u32::wrapping_add(r_b_out, imm);
    }

    self.pc += 4;
  }

  fn mem_relative_w(&mut self, instr : u32){
    // instruction format is
    // 00100aaaaabbbbb?iiiiiiiiiiiiiiii
    // op (5 bits) | r_a (5 bits) | r_b (5 bits) | op (1 bit) | imm (16 bits)

    let r_a = (instr >> 22) & 0x1F;
    let r_b = (instr >> 17) & 0x1F;
    let is_load = ((instr >> 16) & 1) != 0; // is this a load? else is store
    let imm = instr & 0xFFFF;

    // sign extend imm
    let imm = imm | (0xFFFF0000 * ((imm >> 15) & 1));

    // get addr
    let r_b_out = self.regfile[r_b as usize];
    let addr = u32::wrapping_add(r_b_out, imm);

    // make addr pc-relative
    let addr = u32::wrapping_add(addr, self.pc);
    let addr = u32::wrapping_add(addr, 4);

    if is_load {
      if r_a != 0 {
        self.regfile[r_a as usize] = 
        if let Some(data) = self.mem_read32(addr) {
          data
        } else{
          // TLB Miss
          self.raise_tlb_miss(addr);
          return;
        };
      }
    } else {
      // is a store
      let data = self.regfile[r_a as usize];
      if !self.mem_write32(addr, data) {
        // TLB Miss
        self.raise_tlb_miss(addr);
        return;
      }
    }

    self.pc += 4;
  }

  fn mem_imm_w(&mut self, instr : u32){
    // instruction format is
    // 00101aaaaa?iiiiiiiiiiiiiiiiiiiii
    // op (5 bits) | r_a (5 bits) | op (1 bit) | imm (21 bits)

    let r_a = (instr >> 22) & 0x1F;
    let is_load = ((instr >> 21) & 1) != 0; // is this a load? else is store
    let imm = instr & 0x1FFFFF;

    // sign extend imm
    let imm = imm | (0xFFE00000 * ((imm >> 20) & 1));

    // get addr
    let addr = u32::wrapping_add(imm, self.pc);
    let addr = u32::wrapping_add(addr, 4);

    if is_load {
      if r_a != 0 {
        self.regfile[r_a as usize] = 
        if let Some(data) = self.mem_read32(addr) {
          data
        } else{
          // TLB Miss
          self.raise_tlb_miss(addr);
          return;
        };
      }
    } else {
      // is a store
      let data = self.regfile[r_a as usize];
      if !self.mem_write32(addr, data) {
        // TLB Miss
        self.raise_tlb_miss(addr);
        return;
      }
    }

    self.pc += 4;
  }

  fn mem_absolute_d(&mut self, instr : u32){
    // instruction format is
    // 00110aaaaabbbbb?yyzziiiiiiiiiiii
    // op (5 bits) | r_a (5 bits) | r_b (5 bits) | op (1 bit) | y (2 bits) | z (2 bits) | imm (12 bits)

    let r_a = (instr >> 22) & 0x1F;
    let r_b = (instr >> 17) & 0x1F;
    let is_load = ((instr >> 16) & 1) != 0; // is this a load? else is store
    let y = (instr >> 14) & 3;
    let z = (instr >> 12) & 3;
    let imm = instr & 0xFFF;

    // sign extend imm
    let imm = imm | (0xFFFFF000 * ((imm >> 11) & 1));
    // shift imm
    let imm = imm << z;

    if y >= 4 {
      self.raise_exc_instr();
      return;
    };

    // get addr
    let r_b_out = self.regfile[r_b as usize];
    let addr = if y == 2 {r_b_out} else {u32::wrapping_add(r_b_out, imm)}; // check for postincrement

    if is_load {
      if r_a != 0 {
        self.regfile[r_a as usize] = 
        if let Some(data) = self.mem_read16(addr) {
          u32::from(data)
        } else{
          // TLB Miss
          self.raise_tlb_miss(addr);
          return;
        };
      }
    } else {
      // is a store
      let data = self.regfile[r_a as usize];
      if !self.mem_write16(addr, data as u16) {
        // TLB Miss
        self.raise_tlb_miss(addr);
        return;
      }
    }

    if y == 1 || y == 2 && (r_b != 0) {
      // pre or post increment
      self.regfile[r_b as usize] = u32::wrapping_add(r_b_out, imm);
    }

    self.pc += 4;
  }

  fn mem_relative_d(&mut self, instr : u32){
    // instruction format is
    // 00111aaaaabbbbb?iiiiiiiiiiiiiiii
    // op (5 bits) | r_a (5 bits) | r_b (5 bits) | op (1 bit) | imm (16 bits)

    let r_a = (instr >> 22) & 0x1F;
    let r_b = (instr >> 17) & 0x1F;
    let is_load = ((instr >> 16) & 1) != 0; // is this a load? else is store
    let imm = instr & 0xFFFF;

    // sign extend imm
    let imm = imm | (0xFFFF0000 * ((imm >> 15) & 1));

    // get addr
    let r_b_out = self.regfile[r_b as usize];
    let addr = u32::wrapping_add(r_b_out, imm);

    // make addr pc-relative
    let addr = u32::wrapping_add(addr, self.pc);
    let addr = u32::wrapping_add(addr, 4);

    if is_load {
      if r_a != 0 {
        self.regfile[r_a as usize] = 
        if let Some(data) = self.mem_read16(addr) {
          u32::from(data)
        } else{
          // TLB Miss
          self.raise_tlb_miss(addr);
          return;
        };
      }
    } else {
      // is a store
      let data = self.regfile[r_a as usize];
      if !self.mem_write16(addr, data as u16) {
        // TLB Miss
        self.raise_tlb_miss(addr);
        return;
      }
    }

    self.pc += 4;
  }

  fn mem_imm_d(&mut self, instr : u32){
    // instruction format is
    // 01000aaaaa?iiiiiiiiiiiiiiiiiiiii
    // op (5 bits) | r_a (5 bits) | op (1 bit) | imm (21 bits)

    let r_a = (instr >> 22) & 0x1F;
    let is_load = ((instr >> 21) & 1) != 0; // is this a load? else is store
    let imm = instr & 0x1FFFFF;

    // sign extend imm
    let imm = imm | (0xFFE00000 * ((imm >> 20) & 1));

    // get addr
    let addr = u32::wrapping_add(imm, self.pc);
    let addr = u32::wrapping_add(addr, 4);

    if is_load {
      if r_a != 0 {
        self.regfile[r_a as usize] = 
        if let Some(data) = self.mem_read16(addr) {
          u32::from(data)
        } else{
          // TLB Miss
          self.raise_tlb_miss(addr);
          return;
        };
      }
    } else {
      // is a store
      let data = self.regfile[r_a as usize];
      if !self.mem_write16(addr, data as u16) {
        // TLB Miss
        self.raise_tlb_miss(addr);
        return;
      }
    }

    self.pc += 4;
  }

  fn mem_absolute_b(&mut self, instr : u32){
    // instruction format is
    // 01001aaaaabbbbb?yyzziiiiiiiiiiii
    // op (5 bits) | r_a (5 bits) | r_b (5 bits) | op (1 bit) | y (2 bits) | z (2 bits) | imm (12 bits)

    let r_a = (instr >> 22) & 0x1F;
    let r_b = (instr >> 17) & 0x1F;
    let is_load = ((instr >> 16) & 1) != 0; // is this a load? else is store
    let y = (instr >> 14) & 3;
    let z = (instr >> 12) & 3;
    let imm = instr & 0xFFF;

    // sign extend imm
    let imm = imm | (0xFFFFF000 * ((imm >> 11) & 1));
    // shift imm
    let imm = imm << z;

    if y >= 4 {
      self.raise_exc_instr();
      return;
    };

    // get addr
    let r_b_out = self.regfile[r_b as usize];
    let addr = if y == 2 {r_b_out} else {u32::wrapping_add(r_b_out, imm)}; // check for postincrement

    if is_load {
      if r_a != 0 {
        self.regfile[r_a as usize] = 
        if let Some(data) = self.mem_read8(addr) {
          u32::from(data)
        } else{
          // TLB Miss
          self.raise_tlb_miss(addr);
          return;
        };
      }
    } else {
      // is a store
      let data = self.regfile[r_a as usize];
      if !self.mem_write8(addr, data as u8) {
        // TLB Miss
        self.raise_tlb_miss(addr);
        return;
      }
    }

    if (y == 1 || y == 2) && (r_b != 0) {
      // pre or post increment
      self.regfile[r_b as usize] = u32::wrapping_add(r_b_out, imm);
    }

    self.pc += 4;
  }

  fn mem_relative_b(&mut self, instr : u32){
    // instruction format is
    // 01010aaaaabbbbb?iiiiiiiiiiiiiiii
    // op (5 bits) | r_a (5 bits) | r_b (5 bits) | op (1 bit) | imm (16 bits)

    let r_a = (instr >> 22) & 0x1F;
    let r_b = (instr >> 17) & 0x1F;
    let is_load = ((instr >> 16) & 1) != 0; // is this a load? else is store
    let imm = instr & 0xFFFF;

    // sign extend imm
    let imm = imm | (0xFFFF0000 * ((imm >> 15) & 1));

    // get addr
    let r_b_out = self.regfile[r_b as usize];
    let addr = u32::wrapping_add(r_b_out, imm);

    // make addr pc-relative
    let addr = u32::wrapping_add(addr, self.pc);
    let addr = u32::wrapping_add(addr, 4);

    if is_load {
      if r_a != 0 {
        self.regfile[r_a as usize] = 
        if let Some(data) = self.mem_read8(addr) {
          u32::from(data)
        } else{
          // TLB Miss
          self.raise_tlb_miss(addr);
          return;
        };
      }
    } else {
      // is a store
      let data = self.regfile[r_a as usize];
      if !self.mem_write8(addr, data as u8) {
        // TLB Miss
        self.raise_tlb_miss(addr);
        return;
      }
    }

    self.pc += 4;
  }

  fn mem_imm_b(&mut self, instr : u32){
    // instruction format is
    // 01011aaaaa?iiiiiiiiiiiiiiiiiiiii
    // op (5 bits) | r_a (5 bits) | op (1 bit) | imm (21 bits)

    let r_a = (instr >> 22) & 0x1F;
    let is_load = ((instr >> 21) & 1) != 0; // is this a load? else is store
    let imm = instr & 0x1FFFFF;

    // sign extend imm
    let imm = imm | (0xFFE00000 * ((imm >> 20) & 1));

    // get addr
    let addr = u32::wrapping_add(imm, self.pc);
    let addr = u32::wrapping_add(addr, 4);

    if is_load {
      if r_a != 0 {
        self.regfile[r_a as usize] = 
        if let Some(data) = self.mem_read8(addr) {
          u32::from(data)
        } else{
          // TLB Miss
          self.raise_tlb_miss(addr);
          return;
        };
      }
    } else {
      // is a store
      let data = self.regfile[r_a as usize];
      if !self.mem_write8(addr, data as u8) {
        // TLB Miss
        self.raise_tlb_miss(addr);
        return;
      }
    }

    self.pc += 4;
  }

  fn branch_imm(&mut self, instr : u32){
    // instruction format is
    // 01100?????iiiiiiiiiiiiiiiiiiiiii
    // op (5 bits) | op (5 bits) | imm (22 bits)
    let op = (instr >> 22) & 0x1F;
    let imm = instr & 0x3FFFFF;

    // sign extend
    let imm = imm | (0xFFC00000 * ((imm >> 21) & 1));

    let branch = match op {
      0 => true, // br
      1 => self.flags[1], // bz
      2 => !self.flags[1], // bnz
      3 => self.flags[2], // bs
      4 => !self.flags[2], // bns
      5 => self.flags[0], // bc
      6 => !self.flags[0], // bnc
      7 => self.flags[3], // bo
      8 => !self.flags[3], // bno
      9 => !self.flags[1] && !self.flags[2], // bps
      10 => self.flags[1] || self.flags[2], // bnps
      11 => self.flags[2] == self.flags[3] && !self.flags[1], // bg
      12 => self.flags[2] == self.flags[3], // bge
      13 => self.flags[2] != self.flags[3] && !self.flags[1], // bl
      14 => self.flags[2] != self.flags[3] || self.flags[1], // ble
      15 => !self.flags[1] && self.flags[0], // ba
      16 => self.flags[0] || self.flags[1], // bae
      17 => !self.flags[0] && !self.flags[1], // bb
      18 => !self.flags[0] || self.flags[1], // bbe
      _ => {
        self.raise_exc_instr();
        return;
      }
    };

    if branch {
      self.pc = u32::wrapping_add(self.pc, u32::wrapping_add(4 , imm));
    } else {
      self.pc += 4;
    }

  }

  fn branch_absolute(&mut self, instr : u32){
    // instruction format is
    // 01101?????xxxxxxxxxxxxaaaaabbbbb
    // op (5 bits) | op (5 bits) | unused (12 bits) | r_a (5 bits) | r_b (5 bits)
    let op = (instr >> 22) & 0x1F;
    let r_a = (instr >> 5) & 0x1F;
    let r_b = instr & 0x1F;

    // get address
    let r_b = self.regfile[r_b as usize];

    let branch = match op {
      0 => true, // br
      1 => self.flags[1], // bz
      2 => !self.flags[1], // bnz
      3 => self.flags[2], // bs
      4 => !self.flags[2], // bns
      5 => self.flags[0], // bc
      6 => !self.flags[0], // bnc
      7 => self.flags[3], // bo
      8 => !self.flags[3], // bno
      9 => !self.flags[1] && !self.flags[2], // bps
      10 => self.flags[1] || self.flags[2], // bnps
      11 => self.flags[2] == self.flags[3] && !self.flags[1], // bg
      12 => self.flags[2] == self.flags[3], // bge
      13 => self.flags[2] != self.flags[3] && !self.flags[1], // bl
      14 => self.flags[2] != self.flags[3] || self.flags[1], // ble
      15 => !self.flags[1] && self.flags[0], // ba
      16 => self.flags[0] || self.flags[1], // bae
      17 => !self.flags[0] && !self.flags[1], // bb
      18 => !self.flags[0] || self.flags[1], // bbe
      _ => {
        self.raise_exc_instr();
        return;
      }
    };

    if branch {
      if r_a != 0 {
        self.regfile[r_a as usize] = self.pc + 4;
      }
      self.pc = r_b;
    } else {
      self.pc += 4;
    }
  }

  fn branch_relative(&mut self, instr : u32){
    // instruction format is
    // 01110?????xxxxxxxxxxxxaaaaabbbbb
    // op (5 bits) | op (5 bits) | unused (12 bits) | r_a (5 bits) | r_b (5 bits)
    let op = (instr >> 22) & 0x1F;
    let r_a = (instr >> 5) & 0x1F;
    let r_b = instr & 0x1F;

    // get address
    let r_b = self.regfile[r_b as usize];

    let branch = match op {
      0 => true, // br
      1 => self.flags[1], // bz
      2 => !self.flags[1], // bnz
      3 => self.flags[2], // bs
      4 => !self.flags[2], // bns
      5 => self.flags[0], // bc
      6 => !self.flags[0], // bnc
      7 => self.flags[3], // bo
      8 => !self.flags[3], // bno
      9 => !self.flags[1] && !self.flags[2], // bps
      10 => self.flags[1] || self.flags[2], // bnps
      11 => self.flags[2] == self.flags[3] && !self.flags[1], // bg
      12 => self.flags[2] == self.flags[3], // bge
      13 => self.flags[2] != self.flags[3] && !self.flags[1], // bl
      14 => self.flags[2] != self.flags[3] || self.flags[1], // ble
      15 => !self.flags[1] && self.flags[0], // ba
      16 => self.flags[0] || self.flags[1], // bae
      17 => !self.flags[0] && !self.flags[1], // bb
      18 => !self.flags[0] || self.flags[1], // bbe
      _ => {
        self.raise_exc_instr();
        return;
      }
    };

    if branch {
      if r_a != 0 {
        self.regfile[r_a as usize] = self.pc + 4;
      }
      self.pc = u32::wrapping_add(self.pc, u32::wrapping_add(4, r_b));
    } else {
      self.pc += 4;
    }
  }

  fn syscall(&mut self, instr : u32){
    let imm = instr & 0xFF;

    self.kmode = true;
    self.cregfile[0] += 1;

    match imm {
      1 => {
        // sys EXIT

        // save pc and flags
        self.cregfile[4] = self.pc;
        self.cregfile[5] = 
          ((self.flags[3] as u32) << 3) |
          ((self.flags[2] as u32) << 2) |
          ((self.flags[1] as u32) << 1) |
          (self.flags[0] as u32);

        self.pc = self.mem_read32(0x01 * 4).expect("shouldnt fail");
      }
      _ => {
        self.raise_exc_instr();
        return;
      }
    }
  }

  fn update_flags(&mut self, result : u32, lhs : u32, rhs : u32) {
    let result_sign = result >> 31;
    let lhs_sign = lhs >> 31;
    let rhs_sign = rhs >> 31;

    // set the zero flag
    self.flags[1] = result == 0;
    // set the sign flag
    self.flags[2] = result_sign != 0;
    // set the overflow flag
    self.flags[3] = (result_sign != lhs_sign) && (lhs_sign == rhs_sign);
  }


  fn kernel_instr(&mut self, instr : u32){
    if !self.kmode {
      // exec_priv
      assert!(self.cregfile[0] == 0);

      self.save_state();

      self.kmode = true;
      self.cregfile[0] += 1;

      self.pc = self.mem_read32(0x81 * 4).expect("shouldn't fail");
      return;
    }

    assert!(self.cregfile[0] > 0);

    let op = (instr >> 12) & 0x1F;

    match op {
      0 => self.tlb_op(instr),
      1 => self.crmv_op(instr),
      2 => self.mode_op(instr),
      3 => self.rfe(instr),
      _ => {
        self.raise_exc_instr();
        return;
      }
    }
  }

  fn tlb_op(&mut self, instr : u32) {
    let op = (instr >> 10) & 3;
    let ra = (instr >> 22) & 0x1F;
    let rb = (instr >> 17) & 0x1F;

    let rb = self.regfile[rb as usize];
    // rb has pid (12 bits) | addr (20 bits)
    if op == 0 {
      // tlbr
      if ra != 0 {
        if let Some(val) = self.tlb.read(rb) {
          self.regfile[ra as usize] = val;
        } else {
          self.regfile[ra as usize] = 0;
        }
      }
    } else if op == 1 {
      // tlbw
      let ra = self.regfile[ra as usize];
      self.tlb.write(rb, ra & 0x3F);
    } else {
      // tlbc
      self.tlb.clear();
    }
    self.pc += 4;
  }

  fn crmv_op(&mut self, instr : u32) {
    let op = (instr >> 10) & 3;
    let ra = (instr >> 22) & 0x1F;
    let rb = (instr >> 17) & 0x1F;

    if op == 0 {
      // crmv crA, rB
      let rb = self.regfile[rb as usize];
      self.cregfile[ra as usize] = rb;
    } else if op == 1 {
      // crmv rA, crB
      if ra != 0 {
        let rb = self.cregfile[rb as usize];
        self.regfile[ra as usize] = rb;
      }
    } else {
      // crmv crA, crB
      let rb = self.cregfile[rb as usize];
      self.cregfile[ra as usize] = rb;
    }
    self.pc += 4;
  }

  fn mode_op(&mut self, instr : u32) {
    let op = (instr >> 10) & 3;

    if op == 0 {
      // mode run
      self.pc += 4;
    } else if op == 1 {
      // mode sleep
      self.asleep = true;
    } else {
      // mode halt
      self.halted = true;
    }
  }

  fn rfe(&mut self, instr : u32) {
    // update kernel mode
    self.cregfile[0] -= 1;
    if self.cregfile[0] == 0 {
      self.kmode = false;
    }

    if ((instr >> 11) & 1) == 1 {
      // was rfi
      // re-enable interrupts
      self.cregfile[3] |= 0x80000000;
    }

    let ra = (instr >> 22) & 0x1F;
    let rb = (instr >> 17) & 0x1F;

    let ra = self.regfile[ra as usize];
    let rb = self.regfile[rb as usize];

    // restore flags
    self.flags[3] = if (ra >> 3) & 1 != 0 {true} else {false};
    self.flags[2] = if (ra >> 2) & 1 != 0 {true} else {false};
    self.flags[1] = if (ra >> 1) & 1 != 0 {true} else {false};
    self.flags[0] = if (ra >> 0) & 1 != 0 {true} else {false};

    // restore pc
    self.pc = rb;
  }
}