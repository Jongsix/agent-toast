#[cfg(windows)]
pub fn play_notification_sound(sound_name: &str) {
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME};

    // Resolve to full path under C:\Windows\Media\ if it's a bare filename
    let path = if sound_name.contains('\\') || sound_name.contains('/') {
        sound_name.to_string()
    } else {
        format!("C:\\Windows\\Media\\{sound_name}")
    };

    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let pcm = windows::core::PCWSTR(wide.as_ptr());
    unsafe {
        let _ = PlaySoundW(pcm, None, SND_FILENAME | SND_ASYNC);
    }
}

#[cfg(not(windows))]
pub fn play_notification_sound(_sound_name: &str) {}
