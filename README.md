# Virtual Desktop Terminal

一个使用原生 Win32 Desktop Object 的 Windows 虚拟桌面终端工具。

## 功能

- `Ctrl+Alt+F1` 返回程序启动时的默认 Windows Desktop。
- `Ctrl+Alt+F2` 到 `Ctrl+Alt+F6` 按需创建并切换到独立的 Windows Desktop。
- 每个新 Desktop 启动一个由本程序创建的 ConPTY `cmd.exe` 宿主窗口。
- 宿主窗口覆盖整个主屏幕，使用 GDI 绘制终端输出，并吞掉鼠标消息。
- Desktop 只创建一次；切换回来时对应的 `cmd.exe` 进程和终端输出仍然保留。
- 每个终端进程树都放入 Job Object，托盘退出时会回收 `cmd.exe` 及其子进程。
- 默认 Desktop 的系统托盘菜单提供“退出”。如果当前在其他 Desktop，先按 `Ctrl+Alt+1` 返回默认 Desktop。

## 构建与运行

要求 Windows 10 1809 或更高版本，以及 Rust MSVC 工具链：

```powershell
cargo build --release
target\release\virtual_desktop_terminal.exe
```

程序需要在交互式用户会话中运行。Windows Desktop 属于当前 Window Station，跨会话、远程服务会话和安全桌面不在本程序支持范围内。

## 终端实现

宿主使用 ConPTY 连接 `cmd.exe`，键盘输入直接写入伪控制台。输出由 `vt100` 状态机解析，支持 VT/ANSI 的屏幕缓冲、光标定位、清屏、滚动、备用屏幕、颜色、宽字符、inverse 和 underline，因此 `cmd` 的全屏 TUI 可以使用自己的屏幕状态。

窗口使用 Per-Monitor DPI Aware V2。字体大小、字符单元格和 ConPTY 行列数会按窗口 DPI 与尺寸同步调整，并绘制 Linux 风格的闪烁块光标。

## 作者信息

FireGuo（fireguo@flweb.cn）

B站：风梨-FireGuo

本项目使用GPT-5.6制作
