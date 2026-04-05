pub mod lcd;

pub trait Display {
    fn set_status(&mut self, status: &str);
    fn show_qrcode(&mut self, content: &str);
}
