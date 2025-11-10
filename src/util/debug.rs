#[macro_export]
macro_rules! debug_log {
    ($($arg:tt)*) => {
        if true {
            #[cfg(debug_assertions)]
            eprintln!($($arg)*);
        }
    };
}

pub fn attach_console() {
    unsafe {
        use windows_sys::Win32::System::Console::{AllocConsole, FreeConsole};
        use std::io::{self, Write};

        FreeConsole();
        if AllocConsole() != 0 {
            debug_log!("[ignite_overlay] Console attached for debug output.");
            let _ = io::stdout().write_all(b"--- Ignite Overlay Debug Console ---\n");
        }
    }
}