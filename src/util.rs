use widestring::U16CString;

// TODO: Thread-safe Flag?

pub fn is_valid_device_name(name: &str) -> bool {
    name == "Nintendo RVL-CNT-01" || name == "Nintendo RVL-CNT-01-TR"
}

/// Lossily converts a nul-terminated UTF-16 String buffer into a [`String`].
///
/// # Safety
///
/// `buf` must be nul-terminated.
pub unsafe fn wstring_to_utf8(buf: &[u16]) -> String {
    // XXX: Check if nul-terminated and return a Result
    U16CString::from_ptr_str(buf.as_ptr()).to_string_lossy()
}
