use core::ptr::Unique;
use core::mem::{self, size_of};

use super::align_up;

pub struct HoleList {
    first: Hole, // dummy
}

impl HoleList {
    pub const fn empty() -> HoleList {
        HoleList {
            first: Hole {
                size: 0,
                next: None,
            },
        }
    }

    pub unsafe fn new(ptr: *mut Hole, size: usize) -> HoleList {
        assert!(size_of::<Hole>() == Self::min_size());

        mem::forget(mem::replace(&mut *ptr,
                                 Hole {
                                     size: size,
                                     next: None,
                                 }));

        HoleList {
            first: Hole {
                size: 0,
                next: Some(Unique::new(ptr)),
            },
        }
    }

    pub fn allocate_first_fit(&mut self, size: usize, align: usize) -> Option<*mut u8> {
        assert!(size >= Self::min_size());

        allocate_first_fit(&mut self.first, size, align).map(|allocation| {
            if let Some(padding) = allocation.front_padding {
                deallocate(&mut self.first, padding.addr, padding.size);
            }
            if let Some(padding) = allocation.back_padding {
                deallocate(&mut self.first, padding.addr, padding.size);
            }
            allocation.info.addr as *mut u8
        })
    }

    pub fn deallocate(&mut self, ptr: *mut u8, size: usize) {
        println!("deallocate {:p} ({} bytes)", ptr, size);
        assert!(size >= Self::min_size());

        deallocate(&mut self.first, ptr as usize, size)
    }

    pub fn min_size() -> usize {
        size_of::<usize>() * 2
    }

    #[cfg(test)]
    pub fn first_hole(&self) -> Option<(usize, usize)> {
        if let Some(first) = self.first.next.as_ref() {
            Some((**first as usize, unsafe { first.get().size }))
        } else {
            None
        }
    }
}

pub struct Hole {
    pub size: usize,
    pub next: Option<Unique<Hole>>,
}

impl Hole {
    fn info(&self) -> HoleInfo {
        HoleInfo {
            addr: self as *const _ as usize,
            size: self.size,
        }
    }

    /// Returns a reference to the next hole. Panics if this is the last hole.
    fn next_unwrap(&mut self) -> &mut Hole {
        unsafe { self.next.as_mut().unwrap().get_mut() }
    }
}

/// Basic information about a hole.
#[derive(Debug, Clone, Copy)]
struct HoleInfo {
    addr: usize,
    size: usize,
}

/// The result returned by `split_hole` and `allocate_first_fit`. Contains the address and size of
/// the allocation (in the `info` field), and the front and back padding.
struct Allocation {
    info: HoleInfo,
    front_padding: Option<HoleInfo>,
    back_padding: Option<HoleInfo>,
}

fn split_hole(hole: HoleInfo, required_size: usize, required_align: usize) -> Option<Allocation> {
    let aligned_hole = {
        let aligned_hole_addr = align_up(hole.addr, required_align);
        if aligned_hole_addr + required_size > hole.addr + hole.size {
            // hole is too small
            return None;
        }
        HoleInfo {
            addr: aligned_hole_addr,
            size: hole.size - (aligned_hole_addr - hole.addr),
        }
    };

    let front_padding = if aligned_hole.addr == hole.addr {
        // hole has already the required alignment
        None
    } else if aligned_hole.addr < hole.addr + HoleList::min_size() {
        // we can't use this hole because the required padding would create a new, too small hole
        return None;
    } else {
        // the required alignment causes some padding before the allocation
        Some(HoleInfo {
            addr: hole.addr,
            size: aligned_hole.addr - hole.addr,
        })
    };

    let back_padding = if aligned_hole.size == required_size {
        // the aligned hole has exactly the size that's needed, no padding accrues
        None
    } else if aligned_hole.size - required_size < HoleList::min_size() {
        // we can't use this hole since its remains would form a new, too small hole
        return None;
    } else {
        // the hole is bigger than necessary, so there is some padding behind the allocation
        Some(HoleInfo {
            addr: aligned_hole.addr + required_size,
            size: aligned_hole.size - required_size,
        })
    };

    Some(Allocation {
        info: HoleInfo {
            addr: aligned_hole.addr,
            size: required_size,
        },
        front_padding: front_padding,
        back_padding: back_padding,
    })
}

fn allocate_first_fit(previous: &mut Hole, size: usize, align: usize) -> Option<Allocation> {
    previous.next
            .as_mut()
            .and_then(|current| split_hole(unsafe { current.get() }.info(), size, align))
            .map(|allocation| {
                // hole is big enough, so remove it from the list by updating the previous pointer
                previous.next = previous.next_unwrap().next.take();
                allocation
            })
            .or_else(|| {
                // hole is too small, try next hole
                allocate_first_fit(previous.next_unwrap(), size, align)
            })
}

/// Frees the allocation given by `(addr, size)`. It starts at the given hole and walks the list to
/// find the correct place (the list is sorted by address). 
fn deallocate(hole: &mut Hole, addr: usize, size: usize) {
    assert!(size >= HoleList::min_size());

    let hole_addr = if hole.size == 0 {
        // It's the dummy hole, which is the head of the HoleList. It's somewhere on the stack,
        // so it's address is not the address of the hole. We set the addr to 0 as it's always
        // the first hole.
        0
    } else {
        // tt's a real hole in memory and its address is the address of the hole
        hole as *mut _ as usize
    };

    // Each freed block must be handled by the previous hole in memory. Thus the freed address must
    // be always behind the current hole.
    assert!(hole_addr + hole.size <= addr);

    // get information about the next block
    let next_hole_info = hole.next.as_ref().map(|next| unsafe { next.get().info() });

    match next_hole_info {
        Some(next) if hole_addr + hole.size == addr && addr + size == next.addr => {
            // block fills the gap between this hole and the next hole
            // before:  ___XXX____YYYYY____    where X is this hole and Y the next hole
            // after:   ___XXXFFFFYYYYY____    where F is the freed block

            hole.size += size + next.size; // merge the F and Y blocks to this X block
            hole.next = hole.next_unwrap().next.take(); // remove the Y block
        }
        Some(_) if hole_addr + hole.size == addr => {
            // block is right behind this hole but there is used memory after it
            // before:  ___XXX______YYYYY____    where X is this hole and Y the next hole
            // after:   ___XXXFFFF__YYYYY____    where F is the freed block

            hole.size += size; // merge the F block to this X block
        }
        Some(next) if addr + size == next.addr => {
            // block is right before the next hole but there is used memory before it
            // before:  ___XXX______YYYYY____    where X is this hole and Y the next hole
            // after:   ___XXX__FFFFYYYYY____    where F is the freed block

            hole.next = hole.next_unwrap().next.take(); // remove the Y block
            deallocate(hole, addr, size + next.size); // free the merged F/Y block
        }
        Some(next) if next.addr <= addr => {
            // block is behind the next hole, so we delegate it to the next hole
            // before:  ___XXX__YYYYY________    where X is this hole and Y the next hole
            // after:   ___XXX__YYYYY__FFFF__    where F is the freed block

            deallocate(hole.next_unwrap(), addr, size);
        }
        _ => {
            // block is between this and the next hole
            // before:  ___XXX________YYYYY_    where X is this hole and Y the next hole
            // after:   ___XXX__FFFF__YYYYY_    where F is the freed block

            // or: this is the last hole
            // before:  ___XXX_________    where X is this hole
            // after:   ___XXX__FFFF___    where F is the freed block

            let new_hole = Hole {
                size: size,
                next: hole.next.take(), // the reference to the Y block (if it exists)
            };
            // write the new hole to the freed memory
            let ptr = addr as *mut Hole;
            mem::forget(mem::replace(unsafe { &mut *ptr }, new_hole));
            // add the F block as the next block of the X block
            hole.next = Some(unsafe { Unique::new(ptr) });
        }
    }
}