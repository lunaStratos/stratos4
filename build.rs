//! Windows 실행 파일에 아이콘과 버전 정보를 리소스로 심는다.
//! 다른 플랫폼에서는 아무 일도 하지 않는다.
fn main() {
    println!("cargo:rerun-if-changed=img/icon.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut res = winresource::WindowsResource::new();
    res.set_icon("img/icon.ico");
    res.set("ProductName", "stratos4 YT Downloader");
    res.set("FileDescription", "stratos4 YT Downloader");

    // 리소스 컴파일러(rc.exe / windres)가 없는 크로스 환경에서는
    // 아이콘만 빠지고 빌드는 계속되게 둔다.
    if let Err(e) = res.compile() {
        println!("cargo:warning=Windows 리소스를 심지 못했습니다 (아이콘 없이 빌드): {e}");
    }
}
