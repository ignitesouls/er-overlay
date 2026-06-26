#[macro_export]
macro_rules! debug_log {
    ($($arg:tt)*) => {{
        let _ = format_args!($($arg)*);
    }};
}