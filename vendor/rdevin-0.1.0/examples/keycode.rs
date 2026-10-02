fn main() {
    let keycode = rdevin::keycodes::linux::code_from_key(rdevin::Key::Num1);
    dbg!(keycode);
}
