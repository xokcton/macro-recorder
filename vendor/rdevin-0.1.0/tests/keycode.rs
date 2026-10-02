use rdevin::Key as RdevKey;

#[test]
fn test_convet_keycode() {
    let key = RdevKey::KeyQ;
    let (keycode, scancode) = (81, 16);

    assert_eq!(
        key,
        rdevin::keycodes::windows::get_win_key(keycode, scancode)
    );
    assert_eq!(
        (81, 16),
        rdevin::keycodes::windows::get_win_codes(key).unwrap()
    );

    assert_eq!(
        16,
        rdevin::keycodes::windows::scancode_from_key(key).unwrap()
    ); // Windows
    assert_eq!(24, rdevin::keycodes::linux::code_from_key(key).unwrap()); // Linux
    assert_eq!(12, rdevin::keycodes::macos::code_from_key(key).unwrap()); // Mac OS
}
