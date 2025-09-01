use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead};
use std::path::Path;

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
  cregfile : [u32; 7],
  ram : HashMap<u32, u8>,
  tlb : RandomCache,
  pc : u32,
  flags : [bool; 4], // flags are: carry | zero | sign | overflow
  halted : bool
}

fn read_lines<P>(filename: P) -> io::Result<io::Lines<io::BufReader<File>>>
where P: AsRef<Path>, {
    let file = File::open(filename)?;
    Ok(io::BufReader::new(file).lines())
}


impl Emulator {
  pub fn new(path : String) -> Emulator {

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
    
    Emulator {
      kmode: true,
      regfile: [0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0],
      cregfile: [0, 0, 0, 0, 0, 0, 0],
      ram: instructions,
      tlb: RandomCache::new(8),
      pc: 0,
      flags: [false, false, false, false],
      halted: false
    }
  }

  fn mem_write8(&mut self, addr : u32, data : u8) {
    self.ram.insert(addr, data);
  }

  fn mem_write16(&mut self, addr : u32, data : u16) {
    self.mem_write8(addr, data as u8);
    self.mem_write8(addr + 1, (data >> 8) as u8);
  }

  fn mem_write32(&mut self, addr : u32, data : u32) {
    self.mem_write16(addr, data as u16);
    self.mem_write16(addr + 2, (data >> 16) as u16);
  }

  fn mem_read8(&self, addr : u32) -> u8 {
    if self.ram.contains_key(&addr) {
      self.ram[&addr]
    } else {
      0
    }
  }

  fn mem_read16(&self, addr : u32) -> u16 {
    (u16::from(self.mem_read8(addr + 1)) << 8) + 
    u16::from(self.mem_read8(addr))
  }

  fn mem_read32(&self, addr : u32) -> u32 {
    (u32::from(self.mem_read16(addr + 2)) << 16) + 
    u32::from(self.mem_read16(addr))
  }

  pub fn run(&mut self) -> u32 {
    while !self.halted {
      self.execute(self.mem_read32(self.pc));
    }

    // return the value in r3
    self.regfile[3]
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
      _ => panic!("Unrecognized opcode")
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
      _ => panic!("Invalid opcode")
    };

    // never update r0
    if r_a != 0 {
      self.regfile[r_a as usize] = result;
    }
    
    self.update_flags(result, r_b, r_c);

    self.pc += 4;

  }

  fn decode_alu_imm(op : u32, imm : u32) -> u32 {
    match op {
      0..=6 => {
        // Bitwise op
        (imm & 0xFF) << (8 * ((imm >> 8) & 3))
      },
      7..=13 => {
        // Shift op
        imm & 0x1F
      },
      14..=18 => {
        // Arithmetic op
        imm | (0xFFFFF000 * ((imm >> 11) & 1)) // sign extend
      },
      _ => panic!("Unrecognized ALU operation")
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

    let imm = Self::decode_alu_imm(op, imm);

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
      _ => panic!("Invalid opcode")
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

    if y >= 4 {panic!("y must be in range 0..=3 for memory instruction")};

    // get addr
    let r_b_out = self.regfile[r_b as usize];
    let addr = if y == 2 {r_b_out} else {u32::wrapping_add(r_b_out, imm)}; // check for postincrement

    if is_load {
      self.regfile[r_a as usize] = self.mem_read32(addr);
    } else {
      // is a store
      let data = self.regfile[r_a as usize];
      self.mem_write32(addr, data);
    }

    if y == 1 || y == 2 {
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
      self.regfile[r_a as usize] = self.mem_read32(addr);
    } else {
      // is a store
      let data = self.regfile[r_a as usize];
      self.mem_write32(addr, data);
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
      self.regfile[r_a as usize] = self.mem_read32(addr);
    } else {
      // is a store
      let data = self.regfile[r_a as usize];
      self.mem_write32(addr, data);
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

    if y >= 4 {panic!("y must be in range 0..=3 for memory instruction")};

    // get addr
    let r_b_out = self.regfile[r_b as usize];
    let addr = if y == 2 {r_b_out} else {u32::wrapping_add(r_b_out, imm)}; // check for postincrement

    if is_load {
      self.regfile[r_a as usize] = u32::from(self.mem_read16(addr));
    } else {
      // is a store
      let data = self.regfile[r_a as usize];
      self.mem_write16(addr, data as u16);
    }

    if y == 1 || y == 2 {
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
      self.regfile[r_a as usize] = u32::from(self.mem_read16(addr));
    } else {
      // is a store
      let data = self.regfile[r_a as usize];
      self.mem_write16(addr, data as u16);
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
      self.regfile[r_a as usize] = u32::from(self.mem_read16(addr));
    } else {
      // is a store
      let data = self.regfile[r_a as usize];
      self.mem_write16(addr, data as u16);
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

    if y >= 4 {panic!("y must be in range 0..=3 for memory instruction")};

    // get addr
    let r_b_out = self.regfile[r_b as usize];
    let addr = if y == 2 {r_b_out} else {u32::wrapping_add(r_b_out, imm)}; // check for postincrement

    if is_load {
      self.regfile[r_a as usize] = u32::from(self.mem_read8(addr));
    } else {
      // is a store
      let data = self.regfile[r_a as usize];
      self.mem_write8(addr, data as u8);
    }

    if y == 1 || y == 2 {
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
      self.regfile[r_a as usize] = u32::from(self.mem_read8(addr));
    } else {
      // is a store
      let data = self.regfile[r_a as usize];
      self.mem_write8(addr, data as u8);
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
      self.regfile[r_a as usize] = u32::from(self.mem_read8(addr));
    } else {
      // is a store
      let data = self.regfile[r_a as usize];
      self.mem_write8(addr, data as u8);
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
      _ => panic!("Unrecognized branch instruction")
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
      _ => panic!("Unrecognized branch instruction")
    };

    if branch {
      self.regfile[r_a as usize] = self.pc + 4;
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
      _ => panic!("Unrecognized branch instruction")
    };

    if branch {
      self.regfile[r_a as usize] = self.pc + 4;
      self.pc = u32::wrapping_add(self.pc, u32::wrapping_add(4, r_b));
    } else {
      self.pc += 4;
    }
  }

  fn syscall(&mut self, instr : u32){
    let imm = instr & 0x7F;

    match imm {
      0 => {
        // sys EXIT
        self.halted = true;
      }
      _ => panic!("Unrecognized syscall")
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
    }
    let op = (instr >> 12) & 0x1F;

    match op {
      0 => self.tlb_op(instr),
      1 => self.crmv_op(instr),
      2 => self.mode_op(instr),
      3 => self.rfe(instr),
      _ => panic!("Unrecognized opcode")
    }
  }

  fn tlb_op(&mut self, instr : u32) {
    let op = (instr >> 10) & 3;
    let ra = (instr >> 22) & 0x1F;
    let rb = (instr >> 27) & 0x1F;

    let rb = self.regfile[rb as usize];
    if op == 0 {
      // tlbr
      if let Some(val) = self.tlb.read(rb & 0xFFFFF000) {
        self.regfile[ra as usize] = val;
      } else {
        self.regfile[ra as usize] = 0;
      }
    } else if op == 1 {
      // tlbw
      let ra = self.regfile[ra as usize];
      self.tlb.write(rb & 0xFFFFF000, ra & 0xF);
    } else {
      // tlbc
      self.tlb.clear();
    }
  }

  fn crmv_op(&mut self, instr : u32) {
    
  }

  fn mode_op(&mut self, instr : u32) {
    
  }

  fn rfe(&mut self, instr : u32) {
    
    
  }
}