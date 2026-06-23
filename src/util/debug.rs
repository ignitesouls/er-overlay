#[macro_export]
macro_rules! debug_log {
    ($($arg:tt)*) => {{
        let msg = format!("{}\n", format!($($arg)*));

        #[cfg(debug_assertions)]
        {
            eprint!("{}", msg);
        }

        if let Some(dir) = $crate::util::introspection::get_dll_directory() {
            let path = dir.join("ignite_overlay.log");
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                use std::io::Write;
                let _ = file.write_all(msg.as_bytes());
            }
        }

        #[cfg(windows)]
        unsafe {
            use std::ffi::CString;
            use windows_sys::Win32::System::Diagnostics::Debug::OutputDebugStringA;

            if let Ok(cstr) = CString::new(msg) {
                OutputDebugStringA(cstr.as_ptr() as *const u8);
            }
        }
    }};
}

pub fn attach_console() {
    unsafe {
        use std::io::{self, Write};
        use windows_sys::Win32::System::Console::{AllocConsole, FreeConsole};

        FreeConsole();
        if AllocConsole() != 0 {
            debug_log!("[ignite_overlay] Console attached for debug output.");
            let _ = io::stdout().write_all(b"--- Ignite Overlay Debug Console ---\n");
        }
    }
}
