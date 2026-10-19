use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::io;
use std::ptr;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, SetFilePointerEx, FILE_BEGIN, FILE_FLAG_NO_BUFFERING,
    FILE_FLAG_SEQUENTIAL_SCAN, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const GENERIC_READ: u32 = 0x8000_0000;
pub const ALIGN: usize = 4096;

/// Buffer whose address is aligned for FILE_FLAG_NO_BUFFERING.
pub struct AlignedBuf {
    ptr: *mut u8,
    len: usize,
}

unsafe impl Send for AlignedBuf {}

impl AlignedBuf {
    pub fn new(len: usize) -> AlignedBuf {
        let len = len.max(ALIGN).next_multiple_of(ALIGN);
        let layout = Layout::from_size_align(len, ALIGN).expect("layout");
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        AlignedBuf { ptr, len }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        let layout = Layout::from_size_align(self.len, ALIGN).expect("layout");
        unsafe { dealloc(self.ptr, layout) };
    }
}

pub struct Volume {
    handle: HANDLE,
}

// Used by one thread at a time; the handle itself is safe to move between threads.
unsafe impl Send for Volume {}
unsafe impl Sync for Volume {}

impl Volume {
    pub fn open(letter: char) -> io::Result<Volume> {
        let path: Vec<u16> = format!("\\\\.\\{}:", letter)
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::from_raw_os_error(unsafe { GetLastError() } as i32));
        }
        Ok(Volume { handle })
    }

    /// Reads `len` bytes (a multiple of the sector size) at `offset` into the start of the buffer and returns the amount read.
    pub fn read_at(&self, offset: u64, buf: &mut AlignedBuf, len: usize) -> io::Result<usize> {
        if len > buf.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "buffer too small"));
        }
        let mut done: u32 = 0;
        unsafe {
            if SetFilePointerEx(self.handle, offset as i64, ptr::null_mut(), FILE_BEGIN) == 0 {
                return Err(io::Error::last_os_error());
            }
            if ReadFile(
                self.handle,
                buf.ptr as *mut _,
                len as u32,
                &mut done,
                ptr::null_mut(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(done as usize)
    }
}

impl Drop for Volume {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.handle) };
    }
}

pub fn peak_memory_bytes() -> usize {
    unsafe {
        let mut pmc: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        let cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if GetProcessMemoryInfo(GetCurrentProcess(), &mut pmc, cb) != 0 {
            pmc.PeakWorkingSetSize
        } else {
            0
        }
    }
}

pub fn is_elevated() -> bool {
    unsafe {
        let mut token: HANDLE = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut ret: u32 = 0;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut _,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut ret,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}
