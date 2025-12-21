use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead};
use std::path::Path;
use std::cmp;

use std::sync::{Arc, Mutex};
use std::thread;

use crate::memory::{
  Memory, 
  PHYSMEM_MAX, 
  PIT_START, CLK_REG_START,
  SD_INTERRUPT_BIT, VGA_INTERRUPT_BIT
};

use crate::graphics::Graphics;

#[derive(Debug)]
pub struct RandomCache {
    private_table : HashMap<(u32, u32), u32>,
    private_size : usize,
    private_capacity : usize,
    global_table : HashMap<u32, u32>,
    global_size : usize,
    global_capacity : usize,
}

impl RandomCache {
  pub fn new(capacity : usize) -> RandomCache  {
    RandomCache {
      private_table : HashMap::new(),
      private_size : 0,
      private_capacity : capacity,
      global_table : HashMap::new(),
      global_size : 0,
      global_capacity : capacity,
    }
  }

  pub fn access(&self, pid : u32, vpn : u32, operation : u32, kmode : bool) -> Option<u32> {
    // used whenever a memory access is made

    assert!(self.private_size <= self.private_capacity);
    assert!(self.global_size <= self.global_capacity);

    // operations: 0 => read, 1 => write, 2 => fetch

    let key = (pid, vpn);
    let result = self.private_table.get(&key).copied().and_then(|v|
      if operation == 0 {
        // read operation 
        if v & 0x00000001 == 0 {
          // not readable
          None
        } else {
          Some(v)
        }
      } else if operation == 1 {
        // write operation
        if v & 0x00000002 == 0 {
          // not writable
          None
        } else {
          Some(v)
        }
      } else if operation == 2 {
        // fetch operation
        if v & 0x00000004 == 0 {
          // not executable
          None
        } else {
          Some(v)
        }
      } else {
        panic!("invalid operation code");
      }
    ).and_then(|v|
      if !kmode {
        if v & 0x00000008 == 0 {
          // user mode access not allowed
          None
        } else {
          Some(v)
        }
      } else {
        Some(v)
      }
    ).map(|v| v & 0xFFFFF000);

    if result.is_some() {
      return result;
    } else {
      // try global table
      self.global_table.get(&vpn).copied().and_then(|v|
        if operation == 0 {
          // read operation 
          if v & 0x00000001 == 0 {
            // not readable
            None
          } else {
            Some(v)
          }
        } else if operation == 1 {
          // write operation
          if v & 0x00000002 == 0 {
            // not writable
            None
          } else {
            Some(v)
          }
        } else if operation == 2 {
          // fetch operation
          if v & 0x00000004 == 0 {
            // not executable
            None
          } else {
            Some(v)
          }
        } else {
          panic!("invalid operation code");
        }
      ).and_then(|v|
        if !kmode {
          if v & 0x00000008 == 0 {
            // user mode access not allowed
            None
          } else {
            Some(v)
          }
        } else {
          Some(v)
        }
      ).map(|v| v & 0xFFFFF000)
    }
  }

  pub fn read(&self, pid : u32, vpn : u32) -> Option<u32> {
    // used by tlbr instruction

    assert!(self.private_size <= self.private_capacity);
    assert!(self.global_size <= self.global_capacity);
    let result = self.private_table.get(&(pid, vpn)).copied();

    if result.is_some() {
      return result;
    } else {
      // try global table
      self.global_table.get(&vpn).copied()
    }
  }

  pub fn write(&mut self, pid : u32, vpn: u32, ppn : u32){
    if ppn & 0x00000010 != 0 {
      // global entry
      if !self.global_table.contains_key(&vpn) {
        if self.global_size < self.global_capacity {
          self.global_size += 1;
        } else {
          // remove an entry
          let evict = {
            let mut keys = self.global_table.keys();
            keys.next().cloned().expect("size was nonzero, this should work")
          };
          self.global_table.remove(&evict);
        }
      }

      // will replace old mapping if one existed
      self.global_table.insert(vpn, ppn);
      assert!(self.global_size <= self.global_capacity);

    } else {
      // private entry
      if !self.private_table.contains_key(&(pid, vpn)) {
        if self.private_size < self.private_capacity {
          self.private_size += 1;
        } else {
          // remove an entry
          let evict = {
            let mut keys = self.private_table.keys();
            keys.next().cloned().expect("size was nonzero, this should work")
          };
          self.private_table.remove(&evict);
        }
      }

      // will replace old mapping if one existed
      self.private_table.insert((pid, vpn), ppn);

      assert!(self.private_size <= self.private_capacity);
    }
  }

  pub fn invalidate(&mut self, pid : u32, vpn : u32){
    if self.private_table.contains_key(&(pid, vpn)) {
      self.private_size -= 1;
    }
    if self.global_table.contains_key(&vpn) {
      self.global_size -= 1;
    }

    self.private_table.remove(&(pid, vpn));
    self.global_table.remove(&vpn);
  }

  pub fn clear(&mut self){
    self.private_size = 0;
    self.global_size = 0;
    self.private_table.drain();
    self.global_table.drain();
  }
}

pub struct Emulator {
  kmode : bool,
  regfile : [u32; 32], // r0 - r31
  cregfile : [u32; 9], // PSR, PID, ISR, IMR, EPC, FLG, CDV, TLB, KSP
  // in FLG, flags are: carry | zero | sign | overflow
  memory : Memory,
  tlb : RandomCache,
  pc : u32,
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
      cregfile: [1, 0, 0, 0, 0, 0, 0, 0, 0],
      memory: mem,
      tlb: RandomCache::new(8),
      pc: 0x400,
      asleep: false,
      halted: false,
      timer: 0,
      count: 0,
      use_uart_rx: use_uart_rx
    }
  }

  fn convert_mem_address(&self, addr : u32, operation : u32) -> Option<u32> {
    if self.kmode {
      if addr <= PHYSMEM_MAX {
        Some(addr)
      } else if let Some(result) = self.tlb.access(self.cregfile[1], addr >> 12, operation, self.kmode) {
        Some(result | (addr & 0xFFF))
      } else {
        // TLB_KMISS
        None
      }
    } else {
      if let Some(result) = self.tlb.access(self.cregfile[1], addr >> 12, operation, self.kmode) {
        Some(result | (addr & 0xFFF))
      } else {
        // TLB_UMISS
        None
      }
    }
  }

  fn save_state(&mut self){
    // save state as an interrupt happens

    // save pc
    self.cregfile[4] = self.pc;

    // disable interrupts
    self.cregfile[3] &= 0x7FFFFFFF;
  }

  fn raise_tlb_miss(&mut self, addr : u32) {
    // TLB_UMISS = 0x82
    // TLB_KMISS = 0x83

    // save address and pid that caused exception
    self.cregfile[7] = (addr >> 12) | (self.cregfile[1] << 20);

    self.save_state();

    if self.cregfile[0] == u32::MAX {
      panic!("too many nested exceptions!");
    }

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

  // memory operations must be aligned
  fn mem_write8(&mut self, addr : u32, data : u8) -> bool {
    let addr = self.convert_mem_address(addr, 1);

    if let Some(addr) = addr {
      self.memory.write(addr, data);
      true
    } else {
      false
    }
  }

  fn mem_write16(&mut self, addr : u32, data : u16) -> bool {
    if (addr & 1) != 0 {
      // unaligned access
      println!("Warning: unaligned memory access at {:08x}", addr);
    }
    let addr = addr & 0xFFFFFFFE;

    // alignment should mean these return the same value
    let w1 = self.mem_write8(addr, data as u8);
    let w2 = self.mem_write8(addr + 1, (data >> 8) as u8);

    assert!(w1 == w2, "address misaligned or TLB broken");

    return w1;
  }

  fn mem_write32(&mut self, addr : u32, data : u32) -> bool {
    if (addr & 3) != 0 {
      // unaligned access
      println!("Warning: unaligned memory access at {:08x}", addr);
    }

    let addr = addr & 0xFFFFFFFC;

    let w1 = self.mem_write16(addr, data as u16);
    let w2 = self.mem_write16(addr + 2, (data >> 16) as u16);

    assert!(w1 == w2, "address misaligned or TLB broken");

    return w1;
  }

  fn mem_read8(&mut self, addr : u32) -> Option<u8> {
    if addr == 0 {
      println!("Warning: reading from virtual address 0x00000000");
    }

    let addr = self.convert_mem_address(addr, 0);

    if let Some(addr) = addr {
      Some(self.memory.read(addr))
    } else {
      None
    }
  }

  fn mem_read16(&mut self, addr: u32) -> Option<u16> {
    if (addr & 1) != 0 {
      // unaligned access
      println!("Warning: unaligned memory access at {:08x}", addr);
    }
    self.mem_read8(addr).zip(self.mem_read8(addr + 1))
        .map(|(lo, hi)| (u16::from(hi) << 8) | u16::from(lo))
  }

  fn mem_read32(&mut self, addr: u32) -> Option<u32> {
    if (addr & 3) != 0 {
      // unaligned access
      println!("Warning: unaligned memory access at {:08x}", addr);
    }
    self.mem_read16(addr).zip(self.mem_read16(addr + 2))
        .map(|(lo, hi)| (u32::from(hi) << 16) | u32::from(lo))
  }

  fn fetch(&mut self, vaddr: u32) -> Option<u32> {
    if (vaddr & 3) != 0 {
      // unaligned access
      println!("Warning: unaligned memory access at {:08x}", vaddr);
    }
    if vaddr == 0 {
      println!("Warning: fetching from virtual address 0x00000000");
    }

    let paddr = self.convert_mem_address(vaddr, 2);

    if let Some(addr) = paddr {
      Some(
        (self.memory.read(addr + 3) as u32) << 24 |
        (self.memory.read(addr + 2) as u32) << 16 |
        (self.memory.read(addr + 1) as u32) << 8 |
        (self.memory.read(addr) as u32)
      )
    } else {
      None
    }
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
        self.memory.get_vga_mode_register(),
        self.memory.get_vga_status_register(),
        self.memory.get_vga_frame_register(),
        self.memory.get_pending_interrupt()
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

          let clk_divider = 
              (self.memory.read(CLK_REG_START + 3) as u32) << 24 |
              (self.memory.read(CLK_REG_START + 2) as u32) << 16 |
              (self.memory.read(CLK_REG_START + 1) as u32) << 8 |
              (self.memory.read(CLK_REG_START) as u32);

          if !self.asleep && ((self.count % cmp::max(u32::wrapping_add(clk_divider, 1), 1)) == 0) {
            let instr = self.fetch(self.pc);

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
        *ret_clone.lock().unwrap() = Some(self.regfile[1]);
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

    let ints = self.memory.check_interrupts();

    if ints & SD_INTERRUPT_BIT != 0 {
      self.cregfile[2] |= SD_INTERRUPT_BIT;
    }
    if ints & VGA_INTERRUPT_BIT != 0 {
      self.cregfile[2] |= VGA_INTERRUPT_BIT;
    }
    

    // check for timer interrupt
    if self.timer == 0 {
      // check if timer was set
      let old_kmode = self.kmode;
      self.kmode = true;
      let v = (self.memory.read(PIT_START + 3) as u32) << 24 |
              (self.memory.read(PIT_START + 2) as u32) << 16 |
              (self.memory.read(PIT_START + 1) as u32) << 8 |
              (self.memory.read(PIT_START) as u32);
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
        self.pc += 4;
      }
      self.asleep = false;

      self.save_state();

      if self.cregfile[0] == u32::MAX {
        panic!("too many nested exceptions!");
      }

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

    if self.cregfile[0] == u32::MAX {
      panic!("too many nested exceptions!");
    }

    self.kmode = true;
    self.cregfile[0] += 1;

    self.pc = self.mem_read32(0x80 * 4).expect("shouldn't fail");
    return;
  }

  fn execute(&mut self, instr : u32) {
    let opcode = instr >> 27; // opcode is top 5 bits of instruction

    match opcode {
      0 => self.alu_op(instr, false),
      1 => self.alu_op(instr, true),
      2 => self.load_upper_immediate(instr),
      3 => self.mem_absolute(instr, 2),
      4 => self.mem_relative(instr, 2),
      5 => self.mem_imm(instr, 2),
      6 => self.mem_absolute(instr, 1),
      7 => self.mem_relative(instr, 1),
      8 => self.mem_imm(instr, 1),
      9 => self.mem_absolute(instr, 0),
      10 => self.mem_relative(instr, 0),
      11 => self.mem_imm(instr, 0),
      12 => self.branch_imm(instr),
      13 => self.branch_absolute(instr),
      14 => self.branch_relative(instr),
      15 => self.syscall(instr),
      31 => self.kernel_instr(instr),
      _ => self.raise_exc_instr(),
    }
  }

  fn get_reg(&self, regnum : u32) -> u32 {
    if self.kmode && regnum == 31 {
      // use kernel stack pointer
      self.cregfile[8]
    } else {
      // normal register access
      self.regfile[regnum as usize]
    }
  }

  fn write_reg(&mut self, regnum : u32, value : u32) {
    if self.kmode && regnum == 31 {
      // use kernel stack pointer
      self.cregfile[8] = value;
    } else {
      // normal register access
      if regnum != 0 {
        // r0 is always zero
        self.regfile[regnum as usize] = value;
      }
    }
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

  // 2nd operand is either register or immediate
  fn alu_op(&mut self, instr : u32, imm : bool) {
    // instruction format is
    // 00000aaaaabbbbbxxxxxxx?????ccccc
    // op (5 bits) | r_a (5 bits) | r_b (5 bits) | unused (7 bits) | op (5 bits) | r_c (5 bits)
    let r_a = (instr >> 22) & 0x1F;
    let r_b = (instr >> 17) & 0x1F;
    let op = if imm {
      (instr >> 12) & 0x1F
    } else {
      (instr >> 5) & 0x1F
    };

    // retrieve arguments
    let r_b = self.get_reg(r_b);

    let r_c = if imm {
      self.decode_alu_imm(op, instr & 0xFFF).expect("immediate decoding failed")
    } else {
      let r_c = instr & 0x1F;
      self.get_reg(r_c)
    };

    let prev_carry = self.cregfile[5] & 1;

    self.cregfile[5] &= 0xFFFFFFF0; // clear arithmetic flags

    // carry flag is set differently for each instruction,
    // so its handled here. The other flags are all handled together
    let result = match op {
      0 => {
        r_b & r_c // and
      }, 
      1 => {
        !(r_b & r_c)  // nand
      },
      2 => {
        r_b | r_c // or
      },
      3 => {
        !(r_b | r_c) // nor
      },
      4 => {
        r_b ^ r_c // xor
      },
      5 => {
        !(r_b ^ r_c) // xnor
      },
      6 => {
        !r_c // not
      },
      7 => {
        // set carry flag
        self.cregfile[5] |= (r_b >> if r_c > 0 {32 - r_c} else {0} != 0) as u32;
        r_b << r_c // lsl
      },
      8 => {
        // set carry flag
        self.cregfile[5] |= (r_b & ((1 << r_c) - 1) != 0) as u32;
        r_b >> r_c // lsr
      },
      9 => {
        // set carry flag
        let carry = r_b & 1;
        let sign = r_b >> 31;
        self.cregfile[5] |= carry;
        (r_b >> r_c) | (0xFFFFFFFF * sign << if r_c > 0 {32 - r_c} else {0}) // asr
      },
      10 => {
        // set carry flag
        let carry = r_b >> if r_c > 0 {32 - r_c} else {0};
        self.cregfile[5] |= (carry != 0) as u32;
        (r_b << r_c) | carry // rotl
      },
      11 => {
        // set carry flag
        let carry = r_b & ((1 << r_c) - 1);
        self.cregfile[5] |= (carry != 0) as u32;
        (r_b >> r_c) | (carry << if r_c > 0 {32 - r_c} else {0}) // rotr
      },
      12 => {
        // set carry flag
        let carry = if r_c > 0 {r_b >> (32 - r_c)} else {0};
        self.cregfile[5] |= (carry != 0) as u32;
        (r_b << r_c) | if r_c > 0 {prev_carry << (r_c - 1)} else {0} // lslc
      },
      13 => {
        // set carry flag
        let carry = r_b & ((1 << r_c) - 1);
        self.cregfile[5] |= (carry != 0) as u32;
        (r_b >> r_c) | (prev_carry << if r_c > 0 {32 - r_c} else {0}) // lsrc
      },
      14 => {
        // add
        let result = u64::from(r_b) + u64::from(r_c);

        // set the carry flag
        self.cregfile[5] |= (result >> 32 != 0) as u32;

        result as u32
      },
      15 => {
        // addc
        let result = u64::from(r_c) + u64::from(r_b) + u64::from(prev_carry);

        // set the carry flag
        self.cregfile[5] |= (result >> 32 != 0) as u32;

        result as u32
      },
      16 => {
        // sub

        // two's complement
        // sub with immediate does imm - reg
        let result = if imm {
          let r_b = (1 + u64::from(!r_b)) as u32;
          u64::from(r_c) + u64::from(r_b)
        } else {
          let r_c = (1 + u64::from(!r_c)) as u32;
          u64::from(r_c) + u64::from(r_b)
        };

        // set the carry flag
        self.cregfile[5] |= (result >> 32 != 0) as u32;

        result as u32
      },
      17 => {
        // subb

        // two's complement
        let result = if imm {
          let r_b = (1 + u64::from(
          !(u32::wrapping_add(
          u32::from(prev_carry == 0), r_b)))) as u32;
          u64::from(imm) + u64::from(r_b)
        } else {
          let r_c = (1 + u64::from(
          !(u32::wrapping_add(u32::from(prev_carry == 0), r_c)))
          ) as u32;
          u64::from(r_c) + u64::from(r_b)
        };

        // set the carry flag
        self.cregfile[5] |= (result >> 32 != 0) as u32;

        result as u32
      },
      18 => {
        // mul
        let result = u64::from(r_b) * u64::from(r_c);

        // set the carry flag
        self.cregfile[5] |= (result >> 32 != 0) as u32;

        result as u32
      },
      _ => {
        self.raise_exc_instr();
        return;
      }
    };

    // never update r0
    self.write_reg(r_a, result);
    
    self.update_flags(result, r_b, r_c, op);

    self.pc += 4;

  }

  fn load_upper_immediate(&mut self, instr : u32){
    // store imm << 10 in r_a
    let r_a = (instr >> 22) & 0x1F;
    let imm = (instr & 0x03FFFFF) << 10;

    self.write_reg(r_a, imm);

    self.pc += 4;
  }

  fn mem_absolute(&mut self, instr : u32, size : u8){
    // instruction format is
    // 00011aaaaabbbbb?yyzziiiiiiiiiiii
    // op (5 bits) | r_a (5 bits) | r_b (5 bits) | op (1 bit) | y (2 bits) | z (2 bits) | imm (12 bits)

    let r_a = (instr >> 22) & 0x1F;
    let r_b = (instr >> 17) & 0x1F;
    let is_load = ((instr >> 16) & 1) != 0; // is this a load? else is store
    let y = (instr >> 14) & 3; // offset type: 0 = signed offset, 1 = preinc, 2 = postinc, 3 = reserved
    let z = (instr >> 12) & 3; // shift amount
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
    let r_b_out = self.get_reg(r_b);
    let addr = if y == 2 {r_b_out} else {u32::wrapping_add(r_b_out, imm)}; // check for postincrement

    if is_load {
      let data = match size {
        0 => {
          // byte
          self.mem_read8(addr).map(|v| u32::from(v))
        },
        1 => {
          // halfword
          self.mem_read16(addr).map(|v| u32::from(v))
        },
        2 => {
          // word
          self.mem_read32(addr)
        },
        _ => {
          panic!("invalid size for mem instruction");
        }
      };

      if let Some(data) = data {
        self.write_reg(r_a, data);
      } else{
        // TLB Miss
        self.raise_tlb_miss(addr);
        return;
      };
    } else {
      // is a store
      let data = self.get_reg(r_a);
      let success = match size {
        0 => {
          // byte
          self.mem_write8(addr, data as u8)
        },
        1 => {
          // halfword
          self.mem_write16(addr, data as u16)
        },
        2 => {
          // word
          self.mem_write32(addr, data)
        },
        _ => {
          panic!("invalid size for mem instruction");
        }
      };
      if !success {
        // TLB Miss
        self.raise_tlb_miss(addr);
        return;
      }
    }

    if y == 1 || y == 2 {
      // pre or post increment
      self.write_reg(r_b, u32::wrapping_add(r_b_out, imm));
    }

    self.pc += 4;
  }

  fn mem_relative(&mut self, instr : u32, size : u8){
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
    let r_b_out = self.get_reg(r_b);
    let addr = u32::wrapping_add(r_b_out, imm);

    // make addr pc-relative
    let addr = u32::wrapping_add(addr, self.pc);
    let addr = u32::wrapping_add(addr, 4);

    if is_load {
      let data = match size {
        0 => {
          // byte
          self.mem_read8(addr).map(|v| u32::from(v))
        },
        1 => {
          // halfword
          self.mem_read16(addr).map(|v| u32::from(v))
        },
        2 => {
          // word
          self.mem_read32(addr)
        },
        _ => {
          panic!("invalid size for mem instruction");
        }
      };

      if let Some(data) = data {
        self.write_reg(r_a, data);
      } else{
        // TLB Miss
        self.raise_tlb_miss(addr);
        return;
      };
    } else {
      // is a store
      let data = self.get_reg(r_a);

      let success = match size {
        0 => {
          // byte
          self.mem_write8(addr, data as u8)
        },
        1 => {
          // halfword
          self.mem_write16(addr, data as u16)
        },
        2 => {
          // word
          self.mem_write32(addr, data)
        },
        _ => {
          panic!("invalid size for mem instruction");
        }
      };

      if !success {
        // TLB Miss
        self.raise_tlb_miss(addr);
        return;
      }
    }

    self.pc += 4;
  }

  fn mem_imm(&mut self, instr : u32, size : u8){
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
      let data = match size {
        0 => {
          // byte
          self.mem_read8(addr).map(|v| u32::from(v))
        },
        1 => {
          // halfword
          self.mem_read16(addr).map(|v| u32::from(v))
        },
        2 => {
          // word
          self.mem_read32(addr)
        },
        _ => {
          panic!("invalid size for mem instruction");
        }
      };

      if let Some(data) = data {
        self.write_reg(r_a, data);
      } else{
        // TLB Miss
        self.raise_tlb_miss(addr);
        return;
      };
    } else {
      // is a store
      let data = self.get_reg(r_a);

      let success = match size {
        0 => {
          // byte
          self.mem_write8(addr, data as u8)
        },
        1 => {
          // halfword
          self.mem_write16(addr, data as u16)
        },
        2 => {
          // word
          self.mem_write32(addr, data)
        },
        _ => {
          panic!("invalid size for mem instruction");
        }
      };

      if !success {
        // TLB Miss
        self.raise_tlb_miss(addr);
        return;
      }
    }

    self.pc += 4;
  }

  fn get_branch_condition(&mut self, op: u32) -> Option<bool> {
    let carry = (self.cregfile[5] & 1) != 0;
    let zero = (self.cregfile[5] & 2) != 0;
    let sign = (self.cregfile[5] & 4) != 0;
    let overflow = (self.cregfile[5] & 8) != 0;

    match op {
      0 => Some(true), // br
      1 => Some(zero), // bz
      2 => Some(!zero), // bnz
      3 => Some(sign), // bs
      4 => Some(!sign), // bns
      5 => Some(carry), // bc
      6 => Some(!carry), // bnc
      7 => Some(overflow), // bo
      8 => Some(!overflow), // bno
      9 => Some(!zero && !sign), // bps
      10 => Some(zero || sign), // bnps
      11 => Some(sign == overflow && !zero), // bg
      12 => Some(sign == overflow), // bge
      13 => Some(sign != overflow && !zero), // bl
      14 => Some(sign != overflow || zero), // ble
      15 => Some(!zero && carry), // ba
      16 => Some(carry || zero), // bae
      17 => Some(!carry && !zero), // bb
      18 => Some(!carry || zero), // bbe
      _ => {
        self.raise_exc_instr();
        return None;
      }
    }
  }

  fn branch_imm(&mut self, instr : u32){
    // instruction format is
    // 01100?????iiiiiiiiiiiiiiiiiiiiii
    // op (5 bits) | op (5 bits) | imm (22 bits)
    let op = (instr >> 22) & 0x1F;
    let imm = instr & 0x3FFFFF;

    // sign extend
    let imm = imm | (0xFFC00000 * ((imm >> 21) & 1));

    if let Some(branch) = self.get_branch_condition(op) {
      if branch {
        self.pc = u32::wrapping_add(self.pc, u32::wrapping_add(4 , imm));
      } else {
        self.pc += 4;
      }
    } else {
      return;
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
    let r_b = self.get_reg(r_b);

    if let Some(branch) = self.get_branch_condition(op) {
      if branch {
        self.write_reg(r_a, self.pc + 4);
        self.pc = r_b;
      } else {
        self.pc += 4;
      }
    } else {
      return;
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
    let r_b = self.get_reg(r_b);

    if let Some(branch) = self.get_branch_condition(op) {
      if branch {  
        self.write_reg(r_a, self.pc + 4);
        self.pc = u32::wrapping_add(self.pc, u32::wrapping_add(4, r_b));
      } else {
        self.pc += 4;
      }
    } else {
      return;
    }
  }

  fn syscall(&mut self, instr : u32){
    let imm = instr & 0xFF;

    self.kmode = true;
    if self.cregfile[0] == u32::MAX {
      panic!("too many nested exceptions!");
    }
    self.cregfile[0] += 1;

    match imm {
      1 => {
        // sys EXIT

        // save pc and flags
        self.cregfile[4] = self.pc + 4;

        self.pc = self.mem_read32(0x01 * 4).expect("shouldnt fail");
      }
      _ => {
        self.raise_exc_instr();
        return;
      }
    }
  }

  // carry flag handled separately in each alu operation
  fn update_flags(&mut self, result : u32, lhs : u32, rhs : u32, op : u32) {
    let result_sign = result >> 31;
    let lhs_sign = lhs >> 31;
    let rhs_sign = rhs >> 31;

    let is_sub = op == 16 || op == 17;

    // set the zero flag
    self.cregfile[5] |= ((result == 0) as u32) << 1;
    // set the sign flag
    self.cregfile[5] |= ((result_sign != 0) as u32) << 2;
    // set the overflow flag
    self.cregfile[5] |= if is_sub {
      (((result_sign != lhs_sign) && (lhs_sign != rhs_sign)) as u32) << 3
    } else {
      (((result_sign != lhs_sign) && (lhs_sign == rhs_sign)) as u32) << 3
    }
  }


  fn kernel_instr(&mut self, instr : u32){
    if !self.kmode {
      // exec_priv
      assert!(self.cregfile[0] == 0);

      self.save_state();

      self.kmode = true;
      if self.cregfile[0] == u32::MAX {
        panic!("too many nested exceptions!");
      }
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

    let rb = self.get_reg(rb);
    // ra has PPN, rb has VPN
    if op == 0 {
      // tlbr
      if ra != 0 {
        if let Some(val) = self.tlb.read(self.cregfile[1], rb >> 12) {
          self.write_reg(ra, val);
        } else {
          self.write_reg(ra, 0);
        }
      }
    } else if op == 1 {
      // tlbw
      let ra = self.get_reg(ra);
      self.tlb.write(self.cregfile[1], rb >> 12, ra & 0x7FFFFFF);
    } else if op == 2 {
      // tlbi
      self.tlb.invalidate(self.cregfile[1], rb >> 12);
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

    // don't use get_reg/write_reg here because 
    // crmv doesn't respect the r31 => kernel stack pointer alias

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
    } else if op == 2 {
      // crmv crA, crB
      let rb = self.cregfile[rb as usize];
      self.cregfile[ra as usize] = rb;
    } else {
      // crmv rA, rB
      if ra != 0 {
        let rb = self.regfile[rb as usize];
        self.regfile[ra as usize] = rb;
      }
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

    // restore pc
    self.pc = self.cregfile[4];
  }
}
