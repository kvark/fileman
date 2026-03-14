fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("etc/windows/icon.ico");
        res.compile().expect("Failed to compile Windows resources");
    }
}
