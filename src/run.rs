// An efficient Interaction Combinator runtime
// ===========================================
// This file implements an efficient interaction combinator runtime. Nodes are represented by 2 aux
// ports (P1, P2), with the main port (P1) omitted. A separate vector, 'rdex', holds main ports,
// and, thus, tracks active pairs that can be reduced in parallel. Pointers are unboxed, meaning
// that ERAs, NUMs and REFs don't use any additional space. REFs lazily expand to closed nets when
// they interact with nodes, and are cleared when they interact with ERAs, allowing for constant
// space evaluation of recursive functions on Scott encoded datatypes.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

pub type Tag  = u8;
pub type Loc  = u32;
pub type Val  = u64;
pub type AVal = AtomicU64;

// Core terms.
pub const VR1: Tag = 0x0; // Variable to aux port 1
pub const VR2: Tag = 0x1; // Variable to aux port 2
pub const RD1: Tag = 0x2; // Redirect to aux port 1
pub const RD2: Tag = 0x3; // Redirect to aux port 2
pub const REF: Tag = 0x4; // Lazy closed net
pub const ERA: Tag = 0x5; // Unboxed eraser
pub const NUM: Tag = 0x6; // Unboxed number
pub const OP2: Tag = 0x7; // Binary numeric operation
pub const OP1: Tag = 0x8; // Unary numeric operation
pub const MAT: Tag = 0x9; // Numeric pattern-matching
pub const CT0: Tag = 0xA; // Main port of con node, label 0
pub const CT1: Tag = 0xB; // Main port of con node, label 1
pub const CT2: Tag = 0xC; // Main port of con node, label 2
pub const CT3: Tag = 0xD; // Main port of con node, label 3
pub const CT4: Tag = 0xE; // Main port of con node, label 4
pub const CT5: Tag = 0xF; // Main port of con node, label 5

// Numeric operations.
pub const USE: Tag = 0x0; // set-next-op
pub const ADD: Tag = 0x1; // addition
pub const SUB: Tag = 0x2; // subtraction
pub const MUL: Tag = 0x3; // multiplication
pub const DIV: Tag = 0x4; // division
pub const MOD: Tag = 0x5; // modulus
pub const EQ : Tag = 0x6; // equal-to
pub const NE : Tag = 0x7; // not-equal-to
pub const LT : Tag = 0x8; // less-than
pub const GT : Tag = 0x9; // greater-than
pub const AND: Tag = 0xA; // logical-and
pub const OR : Tag = 0xB; // logical-or
pub const XOR: Tag = 0xC; // logical-xor
pub const NOT: Tag = 0xD; // logical-not
pub const LSH: Tag = 0xE; // left-shift
pub const RSH: Tag = 0xF; // right-shift

pub const ERAS: Ptr = Ptr::new(ERA, 0);
pub const ROOT: Ptr = Ptr::new(VR2, 0);
pub const NULL: Ptr = Ptr(0x0000_0000_0000_0000);
pub const GONE: Ptr = Ptr(0xFFFF_FFFF_FFFF_FFEF);
pub const LOCK: Ptr = Ptr(0xFFFF_FFFF_FFFF_FFFF); // if last digit is F it will be seen as a CTR

// An auxiliary port.
pub type Port = Val;
pub const P1: Port = 0;
pub const P2: Port = 1;

// A tagged pointer.
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Hash)]
pub struct Ptr(pub Val);

// An atomic tagged pointer.
pub struct APtr(pub AVal);

// A target pointer, with implied ownership.
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Hash)]
pub enum Trg {
  Dir(Ptr), // we don't own the pointer, so we point to its location
  Ptr(Ptr), // we own the pointer, so we store it directly
}

// The global node buffer.
pub type Data = [(APtr, APtr)];

// A handy wrapper around Data.
pub struct Heap<'a> {
  pub data: &'a Data,
}

// Rewrite counter.
pub struct Rewrites {
  pub anni: usize, // anni rewrites
  pub comm: usize, // comm rewrites
  pub eras: usize, // eras rewrites
  pub dref: usize, // dref rewrites
  pub oper: usize, // oper rewrites
}

// Rewrite counter, atomic.
pub struct AtomicRewrites {
  pub anni: AtomicUsize, // anni rewrites
  pub comm: AtomicUsize, // comm rewrites
  pub eras: AtomicUsize, // eras rewrites
  pub dref: AtomicUsize, // dref rewrites
  pub oper: AtomicUsize, // oper rewrites
}

// A interaction combinator net.
pub struct Net<'a> {
  pub tid : usize, // thread id
  pub tlen: usize, // thread count
  pub heap: Heap<'a>, // nodes
  pub rdex: Vec<(Ptr,Ptr)>, // redexes
  pub locs: Vec<Loc>,
  pub init: usize, // allocation area init index
  pub area: usize, // allocation area size
  pub next: usize, // next allocation index within area
  pub rwts: Rewrites, // rewrite count
}

// A compact closed net, used for dereferences.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct Def {
  pub rdex: Vec<(Ptr, Ptr)>,
  pub node: Vec<(Ptr, Ptr)>,
}

// A map of id to definitions (closed nets).
pub struct Book {
  pub defs: Vec<Def>,
}

impl Ptr {
  #[inline(always)]
  pub const fn new(tag: Tag, loc: Loc) -> Self {
    Ptr(((loc as Val) << 32) | (tag as Val))
  }

  #[inline(always)]
  pub const fn tag(&self) -> Tag {
    (self.0 & 0xF) as Tag
  }

  #[inline(always)]
  pub const fn loc(&self) -> Loc {
    (self.0 >> 32) as Loc
  }

  #[inline(always)]
  pub fn is_nil(&self) -> bool {
    return self.0 == 0;
  }

  #[inline(always)]
  pub fn is_var(&self) -> bool {
    return matches!(self.tag(), VR1..=VR2) && !self.is_nil();
  }

  #[inline(always)]
  pub fn is_red(&self) -> bool {
    return matches!(self.tag(), RD1..=RD2) && !self.is_nil();
  }

  #[inline(always)]
  pub fn is_era(&self) -> bool {
    return matches!(self.tag(), ERA);
  }

  #[inline(always)]
  pub fn is_ctr(&self) -> bool {
    return matches!(self.tag(), CT0..=CT4);
  }

  #[inline(always)]
  pub fn is_ref(&self) -> bool {
    return matches!(self.tag(), REF);
  }

  #[inline(always)]
  pub fn is_pri(&self) -> bool {
    return matches!(self.tag(), REF..=CT4);
  }

  #[inline(always)]
  pub fn is_num(&self) -> bool {
    return matches!(self.tag(), NUM);
  }

  #[inline(always)]
  pub fn is_op1(&self) -> bool {
    return matches!(self.tag(), OP1);
  }

  #[inline(always)]
  pub fn is_op2(&self) -> bool {
    return matches!(self.tag(), OP2);
  }

  #[inline(always)]
  pub fn is_skp(&self) -> bool {
    return matches!(self.tag(), ERA | NUM | REF);
  }

  #[inline(always)]
  pub fn is_mat(&self) -> bool {
    return matches!(self.tag(), MAT);
  }

  #[inline(always)]
  pub fn is_nod(&self) -> bool {
    return matches!(self.tag(), OP2..=CT4);
  }

  #[inline(always)]
  pub fn has_loc(&self) -> bool {
    return matches!(self.tag(), VR1..=VR2 | OP2..=CT4);
  }

  #[inline(always)]
  pub fn redirect(&self) -> Ptr {
    return Ptr::new(self.tag() + RD2 - VR2, self.loc());
  }

  #[inline(always)]
  pub fn unredirect(&self) -> Ptr {
    return Ptr::new(self.tag() + RD2 - VR2, self.loc());
  }

  #[inline(always)]
  pub fn can_skip(a: Ptr, b: Ptr) -> bool {
    return matches!(a.tag(), ERA | REF) && matches!(b.tag(), ERA | REF);
  }
}

impl APtr {
  pub fn new(ptr: Ptr) -> Self {
    APtr(AtomicU64::new(ptr.0))
  }

  pub fn load(&self) -> Ptr {
    Ptr(self.0.load(Ordering::Relaxed))
  }

  pub fn store(&self, ptr: Ptr) {
    self.0.store(ptr.0, Ordering::Relaxed);
  }
}


impl Book {
  #[inline(always)]
  pub fn new() -> Self {
    Book {
      defs: vec![Def::new(); 1 << 24],
    }
  }

  #[inline(always)]
  pub fn def(&mut self, id: Loc, def: Def) {
    self.defs[id as usize] = def;
  }

  #[inline(always)]
  pub fn get(&self, id: Loc) -> Option<&Def> {
    self.defs.get(id as usize)
  }
}

impl Def {
  pub fn new() -> Self {
    Def {
      rdex: vec![],
      node: vec![],
    }
  }
}

impl<'a> Heap<'a> {
  pub fn init(size: usize) -> Box<[(APtr, APtr)]> {
    let mut data = vec![];
    for _ in 0..size {
      data.push((APtr::new(NULL), APtr::new(NULL)));
    }
    return data.into_boxed_slice();
  }

  pub fn new(data: &'a Data) -> Self {
    Heap { data }
  }

  #[inline(always)]
  pub fn get(&self, index: Loc, port: Port) -> Ptr {
    unsafe {
      let node = self.data.get_unchecked(index as usize);
      if port == P1 {
        return node.0.load();
      } else {
        return node.1.load();
      }
    }
  }

  #[inline(always)]
  pub fn set(&self, index: Loc, port: Port, value: Ptr) {
    unsafe {
      let node = self.data.get_unchecked(index as usize);
      if port == P1 {
        node.0.store(value);
      } else {
        node.1.store(value);
      }
    }
  }

  #[inline(always)]
  pub fn cas(&self, index: Loc, port: Port, expected: Ptr, value: Ptr) -> Result<Ptr,Ptr> {
    unsafe {
      let node = self.data.get_unchecked(index as usize);
      let data = if port == P1 { &node.0.0 } else { &node.1.0 };
      let done = data.compare_exchange_weak(expected.0, value.0, Ordering::Relaxed, Ordering::Relaxed);
      return done.map(Ptr).map_err(Ptr);
    }
  }

  pub fn swap(&self, index: Loc, port: Port, value: Ptr) -> Ptr {
    unsafe {
      let node = self.data.get_unchecked(index as usize);
      let data = if port == P1 { &node.0.0 } else { &node.1.0 };
      return Ptr(data.swap(value.0, Ordering::Relaxed));
    }
  }

  #[inline(always)]
  pub fn get_root(&self) -> Ptr {
    return self.get(ROOT.loc(), P2);
  }

  #[inline(always)]
  pub fn set_root(&self, value: Ptr) {
    self.set(ROOT.loc(), P2, value);
  }
}

impl Rewrites {
  pub fn new() -> Self {
    Rewrites {
      anni: 0,
      comm: 0,
      eras: 0,
      dref: 0,
      oper: 0,
    }
  }

  pub fn add_to(&self, target: &AtomicRewrites) {
    target.anni.fetch_add(self.anni, Ordering::Relaxed);
    target.comm.fetch_add(self.comm, Ordering::Relaxed);
    target.eras.fetch_add(self.eras, Ordering::Relaxed);
    target.dref.fetch_add(self.dref, Ordering::Relaxed);
    target.oper.fetch_add(self.oper, Ordering::Relaxed);
  }

}

impl AtomicRewrites {
  pub fn new() -> Self {
    AtomicRewrites {
      anni: AtomicUsize::new(0),
      comm: AtomicUsize::new(0),
      eras: AtomicUsize::new(0),
      dref: AtomicUsize::new(0),
      oper: AtomicUsize::new(0),
    }
  }

  pub fn add_to(&self, target: &mut Rewrites) {
    target.anni += self.anni.load(Ordering::Relaxed);
    target.comm += self.comm.load(Ordering::Relaxed);
    target.eras += self.eras.load(Ordering::Relaxed);
    target.dref += self.dref.load(Ordering::Relaxed);
    target.oper += self.oper.load(Ordering::Relaxed);
  }
}

impl<'a> Net<'a> {
  // Creates an empty net with given size.
  pub fn new(data: &'a Data) -> Self {
    Net {
      tid : 0,
      tlen: 1,
      heap: Heap { data },
      rdex: vec![],
      locs: vec![0; 1 << 16],
      init: 0,
      area: data.len(),
      next: 0,
      rwts: Rewrites::new(),
    }
  }

  // Creates a net and boots from a REF.
  pub fn boot(&mut self, root_id: Loc) {
    self.heap.set_root(Ptr::new(REF, root_id));
  }

  // Total rewrite count.
  pub fn rewrites(&self) -> usize {
    return self.rwts.anni + self.rwts.comm + self.rwts.eras + self.rwts.dref + self.rwts.oper;
  }

  #[inline(always)]
  pub fn alloc(&mut self, size: usize) -> Loc {
    // On the first pass, just alloc without checking.
    // Note: we add 1 to avoid overwritting root.
    if self.next < self.area - 1 {
      self.next += 1;
      return self.init as Loc + self.next as Loc;
    // On later passes, search for an available slot.
    } else {
      loop {
        self.next += 1;
        let index = (self.init + self.next % self.area) as Loc;
        if self.heap.get(index, P2).is_nil() {
          return index;
        }
      }
    }
  }

  #[inline(always)]
  pub fn free(&self, index: Loc) {
    unsafe { self.heap.data.get_unchecked(index as usize) }.0.store(NULL);
    unsafe { self.heap.data.get_unchecked(index as usize) }.1.store(NULL);
  }

  // Gets a pointer's target.
  #[inline(always)]
  pub fn get_target(&self, ptr: Ptr) -> Ptr {
    self.heap.get(ptr.loc(), ptr.0 & 1)
  }

  // Sets a pointer's target.
  #[inline(always)]
  pub fn set_target(&mut self, ptr: Ptr, val: Ptr) {
    self.heap.set(ptr.loc(), ptr.0 & 1, val)
  }

  // Takes a pointer's target.
  #[inline(always)]
  pub fn swap_target(&self, ptr: Ptr, value: Ptr) -> Ptr {
    self.heap.swap(ptr.loc(), ptr.0 & 1, value)
  }

  // Sets a pointer's target, using CAS.
  #[inline(always)]
  pub fn cas_target(&self, ptr: Ptr, expected: Ptr, value: Ptr) -> Result<Ptr,Ptr> {
    self.heap.cas(ptr.loc(), ptr.0 & 1, expected, value)
  }

  #[inline(always)]
  pub fn redux(&mut self, a: Ptr, b: Ptr) {
    if Ptr::can_skip(a, b) {
      self.rwts.eras += 1;
    } else {
      self.rdex.push((a, b));
    }
  }

  #[inline(always)]
  pub fn get(&self, a: Trg) -> Ptr {
    match a {
      Trg::Dir(dir) => self.get_target(dir),
      Trg::Ptr(ptr) => ptr,
    }
  }

  #[inline(always)]
  pub fn swap(&mut self, a: Trg, val: Ptr) -> Ptr {
    match a {
      Trg::Dir(dir) => self.swap_target(dir, val),
      Trg::Ptr(ptr) => ptr,
    }
  }

  // Links two pointers, forming a new wire. Assumes ownership.
  pub fn link(&mut self, a_ptr: Ptr, b_ptr: Ptr) {
    if a_ptr.is_pri() && b_ptr.is_pri() {
      return self.redux(a_ptr, b_ptr);
    } else {
      self.linker(a_ptr, b_ptr);
      self.linker(b_ptr, a_ptr);
    }
  }

  // Given two locations, links both stored pointers, atomically.
  pub fn atomic_link(&mut self, a_dir: Ptr, b_dir: Ptr) {
    //println!("link {:016x} {:016x}", a_dir.0, b_dir.0);
    let a_ptr = self.swap_target(a_dir, LOCK);
    let b_ptr = self.swap_target(b_dir, LOCK);
    if a_ptr.is_pri() && b_ptr.is_pri() {
      self.set_target(a_dir, NULL);
      self.set_target(b_dir, NULL);
      return self.redux(a_ptr, b_ptr);
    } else {
      self.atomic_linker(a_ptr, a_dir, b_ptr);
      self.atomic_linker(b_ptr, b_dir, a_ptr);
    }
  }

  // Given a location, link the pointer stored to another pointer, atomically.
  pub fn half_atomic_link(&mut self, a_dir: Ptr, b_ptr: Ptr) {
    let a_ptr = self.swap_target(a_dir, LOCK);
    if a_ptr.is_pri() && b_ptr.is_pri() {
      self.set_target(a_dir, NULL);
      return self.redux(a_ptr, b_ptr);
    } else {
      self.atomic_linker(a_ptr, a_dir, b_ptr);
      self.linker(b_ptr, a_ptr);
    }
  }

  // When two threads interfere, uses the lock-free link algorithm described on the 'paper/'.
  #[inline(always)]
  pub fn linker(&mut self, a_ptr: Ptr, b_ptr: Ptr) {
    if a_ptr.is_var() {
      self.set_target(a_ptr, b_ptr);
    }
  }

  // When two threads interfere, uses the lock-free link algorithm described on the 'paper/'.
  pub fn atomic_linker(&mut self, a_ptr: Ptr, a_dir: Ptr, b_ptr: Ptr) {
    // If 'a_ptr' is a var...
    if a_ptr.is_var() {
      let got = self.cas_target(a_ptr, a_dir, b_ptr);
      // Attempts to link using a compare-and-swap.
      if got.is_ok() {
        self.set_target(a_dir, NULL);
      // If the CAS failed, resolve by using redirections.
      } else {
        //println!("[{:04x}] cas fail {:016x}", self.tid, got.unwrap_err().0);
        if b_ptr.is_var() {
          self.set_target(a_dir, b_ptr.redirect());
          //self.atomic_linker_var(a_ptr, a_dir, b_ptr);
        } else if b_ptr.is_pri() {
          self.set_target(a_dir, b_ptr);
          self.atomic_linker_pri(a_ptr, a_dir, b_ptr);
        } else {
          todo!();
        }
      }
    } else {
      self.set_target(a_dir, NULL);
    }
  }

  // Atomic linker for when 'b_ptr' is a principal port.
  pub fn atomic_linker_pri(&mut self, mut a_ptr: Ptr, a_dir: Ptr, b_ptr: Ptr) {
    loop {
      // Peek the target, which may not be owned by us.
      let mut t_dir = a_ptr;
      let mut t_ptr = self.get_target(t_dir);
      // If target is a redirection, we own it. Clear and move forward.
      if t_ptr.is_red() {
        self.set_target(t_dir, NULL);
        a_ptr = t_ptr;
        continue;
      }
      // If target is a variable, we don't own it. Try replacing it.
      if t_ptr.is_var() {
        if self.cas_target(t_dir, t_ptr, b_ptr).is_ok() {
          //println!("[{:04x}] var", self.tid);
          // Clear source location.
          self.set_target(a_dir, NULL);
          // Collect the orphaned backward path.
          t_dir = t_ptr;
          t_ptr = self.get_target(t_ptr);
          while t_ptr.is_red() {
            self.swap_target(t_dir, NULL);
            t_dir = t_ptr;
            t_ptr = self.get_target(t_dir);
          }
          return;
        }
        // If the CAS failed, the var changed, so we try again.
        continue;
      }
      // If it is a node, two threads will reach this branch.
      if t_ptr.is_pri() || t_ptr == GONE {
        // Sort references, to avoid deadlocks.
        let x_dir = if a_dir < t_dir { a_dir } else { t_dir };
        let y_dir = if a_dir < t_dir { t_dir } else { a_dir };
        // Swap first reference by GONE placeholder.
        let x_ptr = self.swap_target(x_dir, GONE);
        // First to arrive creates a redex.
        if x_ptr != GONE {
          //println!("[{:04x}] fst {:016x}", self.tid, x_ptr.0);
          let y_ptr = self.swap_target(y_dir, GONE);
          self.redux(x_ptr, y_ptr);
          return;
        // Second to arrive clears up the memory.
        } else {
          //println!("[{:04x}] snd", self.tid);
          self.swap_target(x_dir, NULL);
          while self.cas_target(y_dir, GONE, NULL).is_err() {};
          return;
        }
      }
      // If it is taken, we wait.
      if t_ptr == LOCK {
        continue;
      }
      // Shouldn't be reached.
      //println!("[{:04x}] {:016x} | {:016x} {:016x} {:016x}", self.tid, t_ptr.0, a_dir.0, a_ptr.0, b_ptr.0);
      unreachable!()
    }
  }

  // Atomic linker for when 'b_ptr' is an aux port.
  pub fn atomic_linker_var(&mut self, a_ptr: Ptr, a_dir: Ptr, b_ptr: Ptr) {
    loop {
      let ste_dir = b_ptr;
      let ste_ptr = self.get_target(ste_dir);
      if ste_ptr.is_var() {
        let trg_dir = ste_ptr;
        let trg_ptr = self.get_target(trg_dir);
        if trg_ptr.is_red() {
          let neo_ptr = trg_ptr.unredirect();
          if self.cas_target(ste_dir, ste_ptr, neo_ptr).is_ok() {
            self.swap_target(trg_dir, NULL);
            continue;
          }
        }
      }
      break;
    }
  }

  // Links two targets, using atomics when necessary, based on implied ownership.
  #[inline(always)]
  pub fn safe_link(&mut self, a: Trg, b: Trg) {
    match (a, b) {
      (Trg::Dir(a_dir), Trg::Dir(b_dir)) => self.atomic_link(a_dir, b_dir),
      (Trg::Dir(a_dir), Trg::Ptr(b_ptr)) => self.half_atomic_link(a_dir, b_ptr),
      (Trg::Ptr(a_ptr), Trg::Dir(b_dir)) => self.half_atomic_link(b_dir, a_ptr),
      (Trg::Ptr(a_ptr), Trg::Ptr(b_ptr)) => self.link(a_ptr, b_ptr),
    }
  }
  
  // Performs an interaction over a redex.
  #[inline(always)]
  pub fn interact(&mut self, book: &Book, a: Ptr, b: Ptr) {
    //println!("{:016x} {:016x}", a.0, b.0);
    match (a.tag(), b.tag()) {
      (REF   , OP2..) => self.call(book, a, b),
      (OP2.. , REF  ) => self.call(book, b, a),
      (CT0.. , CT0..) if a.tag() == b.tag() => self.anni(a, b),
      (CT0.. , CT0..) => self.comm(a, b),
      (CT0.. , ERA  ) => self.era2(a),
      (ERA   , CT0..) => self.era2(b),
      (REF   , ERA  ) => self.rwts.eras += 1,
      (ERA   , REF  ) => self.rwts.eras += 1,
      (ERA   , ERA  ) => self.rwts.eras += 1,
      (CT0.. , NUM  ) => self.copy(a, b),
      (NUM   , CT0..) => self.copy(b, a),
      (NUM   , ERA  ) => self.rwts.eras += 1,
      (ERA   , NUM  ) => self.rwts.eras += 1,
      (NUM   , NUM  ) => self.rwts.eras += 1,
      (OP2   , NUM  ) => self.op2n(a, b),
      (NUM   , OP2  ) => self.op2n(b, a),
      (OP1   , NUM  ) => self.op1n(a, b),
      (NUM   , OP1  ) => self.op1n(b, a),
      (OP2   , CT0..) => self.comm(a, b),
      (CT0.. , OP2  ) => self.comm(b, a),
      (OP1   , CT0..) => self.pass(a, b),
      (CT0.. , OP1  ) => self.pass(b, a),
      (OP2   , ERA  ) => self.era2(a),
      (ERA   , OP2  ) => self.era2(b),
      (OP1   , ERA  ) => self.era1(a),
      (ERA   , OP1  ) => self.era1(b),
      (MAT   , NUM  ) => self.mtch(a, b),
      (NUM   , MAT  ) => self.mtch(b, a),
      (MAT   , CT0..) => self.comm(a, b),
      (CT0.. , MAT  ) => self.comm(b, a),
      (MAT   , ERA  ) => self.era2(a),
      (ERA   , MAT  ) => self.era2(b),
      _               => unreachable!(),
    };
  }

  pub fn anni(&mut self, a: Ptr, b: Ptr) {
    self.rwts.anni += 1;
    let a1 = Ptr::new(VR1, a.loc());
    let b1 = Ptr::new(VR1, b.loc());
    self.atomic_link(a1, b1);
    let a2 = Ptr::new(VR2, a.loc());
    let b2 = Ptr::new(VR2, b.loc());
    self.atomic_link(a2, b2);
  }

  pub fn comm(&mut self, a: Ptr, b: Ptr) {
    self.rwts.comm += 1;
    let loc0 = self.alloc(1);
    let loc1 = self.alloc(1);
    let loc2 = self.alloc(1);
    let loc3 = self.alloc(1);
    self.heap.set(loc0, P1, Ptr::new(VR1, loc2));
    self.heap.set(loc0, P2, Ptr::new(VR1, loc3));
    self.heap.set(loc1, P1, Ptr::new(VR2, loc2));
    self.heap.set(loc1, P2, Ptr::new(VR2, loc3));
    self.heap.set(loc2, P1, Ptr::new(VR1, loc0));
    self.heap.set(loc2, P2, Ptr::new(VR1, loc1));
    self.heap.set(loc3, P1, Ptr::new(VR2, loc0));
    self.heap.set(loc3, P2, Ptr::new(VR2, loc1));
    let a1 = Ptr::new(VR1, a.loc());
    self.half_atomic_link(a1, Ptr::new(b.tag(), loc0));
    let b1 = Ptr::new(VR1, b.loc());
    self.half_atomic_link(b1, Ptr::new(a.tag(), loc2));
    let a2 = Ptr::new(VR2, a.loc());
    self.half_atomic_link(a2, Ptr::new(b.tag(), loc1));
    let b2 = Ptr::new(VR2, b.loc());
    self.half_atomic_link(b2, Ptr::new(a.tag(), loc3));
  }

  pub fn era2(&mut self, a: Ptr) {
    self.rwts.eras += 1;
    let a1 = Ptr::new(VR1, a.loc());
    self.half_atomic_link(a1, ERAS);
    let a2 = Ptr::new(VR2, a.loc());
    self.half_atomic_link(a2, ERAS);
  }

  pub fn era1(&mut self, a: Ptr) {
    self.rwts.eras += 1;
    let a2 = Ptr::new(VR2, a.loc());
    self.half_atomic_link(a2, ERAS);
  }

  pub fn pass(&mut self, a: Ptr, b: Ptr) {
    self.rwts.comm += 1;
    let loc0 = self.alloc(1);
    let loc1 = self.alloc(1);
    let loc2 = self.alloc(1);
    self.heap.set(loc0, P1, Ptr::new(VR2, loc1));
    self.heap.set(loc0, P2, Ptr::new(VR2, loc2));
    self.heap.set(loc1, P1, self.heap.get(a.loc(), P1));
    self.heap.set(loc1, P2, Ptr::new(VR1, loc0));
    self.heap.set(loc2, P1, self.heap.get(a.loc(), P1));
    self.heap.set(loc2, P2, Ptr::new(VR2, loc0));
    let a2 = Ptr::new(VR2, a.loc());
    self.half_atomic_link(a2, Ptr::new(b.tag(), loc0));
    let b1 = Ptr::new(VR1, b.loc());
    self.half_atomic_link(b1, Ptr::new(a.tag(), loc1));
    let b2 = Ptr::new(VR2, b.loc());
    self.half_atomic_link(b2, Ptr::new(a.tag(), loc2));
  }

  pub fn copy(&mut self, a: Ptr, b: Ptr) {
    self.rwts.comm += 1;
    let a1 = Ptr::new(VR1, a.loc());
    self.half_atomic_link(a1, b);
    let a2 = Ptr::new(VR2, a.loc());
    self.half_atomic_link(a2, b);
  }

  pub fn mtch(&mut self, a: Ptr, b: Ptr) {
    self.rwts.oper += 1;
    let a1 = Ptr::new(VR1, a.loc()); // branch
    let a2 = Ptr::new(VR1, a.loc()); // return
    if b.loc() == 0 {
      let loc0 = self.alloc(1);
      self.heap.set(loc0, P2, ERAS);
      self.half_atomic_link(a1, Ptr::new(CT0, loc0));
      self.half_atomic_link(a2, Ptr::new(VR1, loc0));
    } else {
      let loc0 = self.alloc(1);
      let loc1 = self.alloc(1);
      self.heap.set(loc0, P1, ERAS);
      self.heap.set(loc0, P2, Ptr::new(CT0, loc1));
      self.heap.set(loc1, P1, Ptr::new(NUM, b.loc() - 1));
      self.half_atomic_link(a1, Ptr::new(CT0, loc0));
      self.half_atomic_link(a2, Ptr::new(VR2, loc1));
    }
  }

  pub fn op2n(&mut self, a: Ptr, b: Ptr) {
    self.rwts.oper += 1;
    let a1 = Ptr::new(VR1, a.loc());
    self.half_atomic_link(a1, Ptr::new(OP1, a.loc()));
    self.heap.set(a.loc(), P1, b);
  }

  pub fn op1n(&mut self, a: Ptr, b: Ptr) {
    self.rwts.oper += 1;
    let v0 = self.heap.get(a.loc(), P1).loc() as Loc;
    let v1 = b.loc() as Loc;
    let v2 = self.op(v0, v1);
    let a2 = Ptr::new(VR2, a.loc());
    self.half_atomic_link(a2, Ptr::new(NUM, v2));
  }

  #[inline(always)]
  pub fn op(&self, a: Loc, b: Loc) -> Loc {
    let a_opr = (a >> 24) & 0xF;
    let b_opr = (b >> 24) & 0xF; // not used yet
    let a_val = a & 0xFFFFFF;
    let b_val = b & 0xFFFFFF;
    match a_opr as Tag {
      USE => { ((a_val & 0xF) << 24) | b_val }
      ADD => { (a_val.wrapping_add(b_val)) & 0xFFFFFF }
      SUB => { (a_val.wrapping_sub(b_val)) & 0xFFFFFF }
      MUL => { (a_val.wrapping_mul(b_val)) & 0xFFFFFF }
      DIV => { if b_val == 0 { 0xFFFFFF } else { (a_val.wrapping_div(b_val)) & 0xFFFFFF } }
      MOD => { (a_val.wrapping_rem(b_val)) & 0xFFFFFF }
      EQ  => { ((a_val == b_val) as Loc) & 0xFFFFFF }
      NE  => { ((a_val != b_val) as Loc) & 0xFFFFFF }
      LT  => { ((a_val < b_val) as Loc) & 0xFFFFFF }
      GT  => { ((a_val > b_val) as Loc) & 0xFFFFFF }
      AND => { (a_val & b_val) & 0xFFFFFF }
      OR  => { (a_val | b_val) & 0xFFFFFF }
      XOR => { (a_val ^ b_val) & 0xFFFFFF }
      NOT => { (!b_val) & 0xFFFFFF }
      LSH => { (a_val << b_val) & 0xFFFFFF }
      RSH => { (a_val >> b_val) & 0xFFFFFF }
      _   => { unreachable!() }
    }
  }


  // Expands a closed net.
  #[inline(always)]
  pub fn call(&mut self, book: &Book, ptr: Ptr, par: Ptr) {
    self.rwts.dref += 1;
    let mut ptr = ptr;
    // FIXME: change "while" to "if" once lang prevents refs from returning refs
    if ptr.is_ref() {
      // Intercepts with a native function, if available.
      if self.call_native(book, ptr, par) {
        return;
      }
      // Load the closed net.
      let got = unsafe { book.defs.get_unchecked((ptr.loc() as usize) & 0xFFFFFF) };
      if got.node.len() > 0 {
        let len = got.node.len() - 1;
        // Allocate space.
        for i in 0 .. len {
          *unsafe { self.locs.get_unchecked_mut(1 + i) } = self.alloc(1);
        }
        // Load nodes, adjusted.
        for i in 0 .. len {
          let p1 = self.adjust(unsafe { got.node.get_unchecked(1 + i) }.0);
          let p2 = self.adjust(unsafe { got.node.get_unchecked(1 + i) }.1);
          let lc = *unsafe { self.locs.get_unchecked(1 + i) };
          self.heap.set(lc, P1, p1);
          self.heap.set(lc, P2, p2);
        }
        // Load redexes, adjusted.
        for r in &got.rdex {
          let p1 = self.adjust(r.0);
          let p2 = self.adjust(r.1);
          self.rdex.push((p1, p2));
        }
        // Load root, adjusted.
        ptr = self.adjust(got.node[0].1);
      }
    }
    self.link(ptr, par);
  }

  // Adjusts dereferenced pointer locations.
  fn adjust(&self, ptr: Ptr) -> Ptr {
    if ptr.has_loc() {
      return Ptr::new(ptr.tag(), *unsafe { self.locs.get_unchecked(ptr.loc() as usize) });
    } else {
      return ptr;
    }
  }

  // Reduces all redexes.
  #[inline(always)]
  pub fn reduce(&mut self, book: &Book) {
    let mut rdex: Vec<(Ptr, Ptr)> = vec![];
    std::mem::swap(&mut self.rdex, &mut rdex);
    while rdex.len() > 0 {
      for (a, b) in &rdex {
        self.interact(book, *a, *b);
      }
      rdex.clear();
      std::mem::swap(&mut self.rdex, &mut rdex);
    }
  }

  // Expands heads.
  #[inline(always)]
  pub fn expand(&mut self, book: &Book) {
    fn go(net: &mut Net, book: &Book, dir: Ptr, len: usize, key: usize) {
      //println!("[{:04x}] expand dir: {:016x}", net.tid, dir.0);
      let ptr = net.get_target(dir);
      if ptr.is_ctr() {
        if len >= net.tlen || key % 2 == 0 {
          go(net, book, Ptr::new(VR1, ptr.loc()), len * 2, key / 2);
        }
        if len >= net.tlen || key % 2 == 1 {
          go(net, book, Ptr::new(VR2, ptr.loc()), len * 2, key / 2);
        }
      } else if ptr.is_ref() {
        let got = net.swap_target(dir, LOCK);
        if got != LOCK {
          //println!("[{:08x}] expand {:08x}", net.tid, dir.0);
          net.call(book, ptr, dir);
        }
      }
    }
    return go(self, book, ROOT, 1, self.tid);
  }

  // Reduce a net to normal form.
  //pub fn normal(&mut self, book: &Book) {
    //self.expand(book);
    //while self.rdex.len() > 0 {
      //self.reduce(book);
      //self.expand(book);
    //}
  //}

  // Forks into child threads, returning a Net for the (tid/tlen)'th thread.
  pub fn fork(&self, tid: usize, tlen: usize) -> Self {
    let mut net = Net::new(self.heap.data);
    net.tid  = tid;
    net.tlen = tlen;
    net.init = self.heap.data.len() * tid / tlen;
    net.area = self.heap.data.len() / tlen;
    let from = self.rdex.len() * (tid + 0) / tlen;
    let upto = self.rdex.len() * (tid + 1) / tlen;
    for i in from .. upto {
      let r = self.rdex[i];
      let x = r.0;
      let y = r.1;
      net.rdex.push((x,y));
    }
    if tid == 0 {
      net.next = self.next;
    }
    return net;
  }

  pub fn normal(&mut self, book: &Book) {
    let tlen_l2 = 3;
    let tlen    = 1 << tlen_l2;

    const STLEN : usize = 65536; // max steal redexes / split 

    // Global values
    let delta = AtomicRewrites::new(); // delta rewrite counter
    let steal = &mut vec![]; // steal buffer for redex exchange
    let rlens = &mut vec![]; // length of each tid's redex bags
    let total = AtomicUsize::new(0); // sum of redex bag length
    let barry = Arc::new(Barrier::new(tlen)); // global barrier

    // Initializes the rlens buffer
    for i in 0 .. tlen {
      rlens.push(AtomicUsize::new(0x4321_FFFF_FFFF_FFFF));
    }
    
    // Initializes the steal buffer
    for i in 0 .. STLEN * tlen {
      steal.push((AtomicU64::new(0x1234_FFFF_FFFF_FFFF), AtomicU64::new(0x1234_FFFF_FFFF_FFFF)));
    }

    // Creates a thread scope
    std::thread::scope(|s| {

      // For each thread...
      for tid in 0 .. tlen {

        // Creates thread local attributes
        let     delta = &delta;
        let     steal = &steal;
        let     rlens = &rlens;
        let     total = &total;
        let     barry = Arc::clone(&barry);
        let mut tick  = 0;
        //let mut rbuff = vec![];
        let mut child = self.fork(tid, tlen);

        // Spawns the thread
        s.spawn(move || {

          // Parallel reduction loop
          loop {

            // Synchronizes threads
            barry.wait();

            //println!("[{:08x}] reducing {}", tid, child.rdex.len());

            // Rewrites current redexes
            child.reduce(book);

            // Expands if redex count is 0
            rlens[tid].store(child.rdex.len(), Ordering::Relaxed);
            total.fetch_add(child.rdex.len(), Ordering::Relaxed);
            barry.wait();
            if total.load(Ordering::Relaxed) == 0 {
              child.expand(book);
            }
            barry.wait();
            total.store(0, Ordering::Relaxed);
            barry.wait();

            // Halts if redex count is still 0
            rlens[tid].store(child.rdex.len(), Ordering::Relaxed);
            total.fetch_add(child.rdex.len(), Ordering::Relaxed);
            barry.wait();
            if total.load(Ordering::Relaxed) == 0 {
              break;
            }
            barry.wait();
            total.store(0, Ordering::Relaxed);

            // Shares redexes with target thread
            let side  = (child.tid >> (tlen_l2 - 1 - (tick % tlen_l2))) & 1;
            let shift = (1 << (tlen_l2 - 1)) >> (tick % tlen_l2);
            let b_tid = if side == 1 { child.tid - shift } else { child.tid + shift };
            let a_len = child.rdex.len();
            let b_len = rlens[b_tid].load(Ordering::Relaxed);
            if a_len > b_len {
              for i in 0 .. (a_len - b_len) / 2 { // TODO: avoid reversing
                let r = child.rdex.pop().unwrap();
                steal[b_tid * STLEN + i].0.store(r.0.0, Ordering::Relaxed);
                steal[b_tid * STLEN + i].1.store(r.1.0, Ordering::Relaxed);
              }
            }
            barry.wait();
            if b_len > a_len {
              for i in 0 .. (b_len - a_len) / 2 {
                let r = &steal[tid * STLEN + i];
                let x = Ptr(r.0.load(Ordering::Relaxed));
                let y = Ptr(r.1.load(Ordering::Relaxed));
                child.rdex.push((x, y));
              }
            }

            // Incs tick
            tick += 1;
          }

          // Adds rewrites to stats
          child.rwts.add_to(delta);
        });
      }
    });

    self.rdex.clear();
    delta.add_to(&mut self.rwts);

    println!("ALL DONE");

  }
}
