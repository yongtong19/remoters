# LAN Remote

Mac 和 Windows 共用一套 Rust 桌面客户端。两端都有 **Connect**（主控）和 **Share**（被控）入口，可在同一局域网内通过 IPv4 直连。

## 使用方式

1. 在被控电脑打开 **Share**，选择与主控电脑同网的 IPv4 地址，点 **Start sharing**。勾选 **View only** 可只共享画面。
2. 在主控电脑打开 **Connect**，填入被控端显示的地址和配对码，点 **Connect**。默认端口为 `47821`。
3. 要发送鼠标和键盘输入，点击 **Take control**。按 `Ctrl + Alt/Option + Esc` 或移出窗口焦点即可释放输入。
4. 被控端点击 **Stop sharing** 可结束共享。配对码在每次重新开始共享时更新。

macOS 14 或更新版本作为被控端时，需要在系统设置中允许屏幕录制。启用键鼠控制还需要辅助功能权限。两台设备都需要保持登录。连接超时时，检查被控端防火墙是否允许来自主控设备的 TCP 连接。

## 当前实现

| 部分 | 代码与职责 |
| --- | --- |
| `crates/remote-core` | Rust。封包、配对、屏幕会话、键鼠事件，以及 TCP/TLS 直连。 |
| `crates/desktop` | Rust + GPUI。Mac、Windows 共用同一个 UI 和主控/被控逻辑。 |
| 平台适配 | macOS 用 ScreenCaptureKit，Windows 用 XCap 采集主屏。两端通过 Enigo 注入键鼠，并按系统转换坐标和键位。 |

每次共享使用临时 TLS 身份。配对码包含证书 SHA‑256 指纹和随机口令。主控验证被控端持有对应证书的私钥，然后通过加密连接发送口令。只有认证成功后才会打开桌面会话。

画面采用 JPEG，缩放到 1440 × 1000 以内，约 15 帧/秒的上限。首版支持单主屏、常用键鼠和一个活动会话。系统保留快捷键、锁屏和系统安全桌面取决于操作系统权限。

## 构建

Windows 或 macOS 使用 Rust 1.92 或更新版本。Windows 需要 MSVC C++ 构建工具和带有 FXC 的 Windows SDK。若 SDK 装在其他目录，构建前将 `GPUI_FXC_PATH` 指向 x64 的 `fxc.exe`。macOS 需要 Xcode 的 Swift 工具链和 macOS SDK。

```sh
cargo test --workspace --locked
cargo build --release --locked -p lan-remote
```

Windows 的程序在 `target/release/lan-remote.exe`。在 macOS 上可将构建结果打包为本地签名的应用，以便授予系统权限。

```sh
bash packaging/macos.sh
open target/release/LanRemote.app
```

提交和拉取请求会自动构建 [Windows x64](https://github.com/yongtong19/lan-remote/actions/workflows/desktop.yml)。日常在本机验证 Mac。需要 Apple Silicon 构建产物时，可手动运行 [macOS 工作流](https://github.com/yongtong19/lan-remote/actions/workflows/macos.yml)。

## 后续阶段

1. **Ubuntu**。复用桌面 UI 和会话，分别适配 X11 和 Wayland 的屏幕采集与输入授权。
2. **Android 主控**。复用 Rust 核心，优先复用界面，按需适配触摸和 Android 生命周期。
3. **跨网连接**。新增独立的公网连接协调服务，交换打洞候选地址。直连失败时使用中转，桌面和输入数据仍由端到端会话加密。局域网直连路径继续独立运行。
