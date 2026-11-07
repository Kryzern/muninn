// Embeds the icon and version info into muninn.exe.
fn main() {
    let v = env!("CARGO_PKG_VERSION");
    let mut n = v.split('.').map(|x| x.parse::<u16>().unwrap_or(0));
    let (a, b, c) = (n.next().unwrap_or(0), n.next().unwrap_or(0), n.next().unwrap_or(0));
    let rc = format!(
        r#"1 ICON "packaging/muninn.ico"
1 VERSIONINFO
FILEVERSION {a},{b},{c},0
PRODUCTVERSION {a},{b},{c},0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Ethan Dsouza (Kryzern)"
      VALUE "FileDescription", "Muninn"
      VALUE "FileVersion", "{v}"
      VALUE "LegalCopyright", "MIT License"
      VALUE "OriginalFilename", "muninn.exe"
      VALUE "ProductName", "Muninn"
      VALUE "ProductVersion", "{v}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let out = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("muninn.rc");
    std::fs::write(&out, rc).unwrap();
    embed_resource::compile(&out, embed_resource::NONE).manifest_optional().unwrap();
    println!("cargo:rerun-if-changed=packaging/muninn.ico");
}
