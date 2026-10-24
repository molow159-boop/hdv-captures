fn main() {
    #[cfg(target_os = "windows")]
    {
        let mut resource = winres::WindowsResource::new();
        resource.set_icon("assets/icon.ico");
        resource.set("ProductName", "HDV Captures");
        resource.set("Comments", "Version 1.1.0");
        resource.set("FileDescription", "Capture locale des fiches HDV");
        resource
            .compile()
            .expect("Impossible d'inclure l'icône Windows");
    }
}
