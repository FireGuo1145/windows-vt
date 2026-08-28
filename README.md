# Virtual Desktop Terminal

一个使用原生 Win32 Desktop Object 的 Windows 虚拟桌面终端工具。

## 功能

- `Ctrl+Alt+1` 返回程序启动时的默认 Windows Desktop。
- `Ctrl+Alt+2` 到 `Ctrl+Alt+6` 按需创建并切换到独立的 Windows Desktop。
- 每个新 Desktop 启动一个由本程序创建的 ConPTY `cmd.exe` 宿主窗口。
- 宿主窗口覆盖整个主屏幕，使用 GDI 绘制终端输出，并吞掉鼠标消息。
- Desktop 只创建一次；切换回来时对应的 `cmd.exe` 进程和终端输出仍然保留。
- 默认 Desktop 的系统托盘菜单提供“退出”。如果当前在其他 Desktop，先按 `Ctrl+Alt+1` 返回默认 Desktop。

## 构建与运行

要求 Windows 10 1809 或更高版本，以及 Rust MSVC 工具链：

```powershell
cargo build --release
target\release\virtual_desktop_terminal.exe
```

程序需要在交互式用户会话中运行。Windows Desktop 属于当前 Window Station，跨会话、远程服务会话和安全桌面不在本程序支持范围内。

## 终端实现边界

宿主使用 ConPTY 连接 `cmd.exe`，键盘输入直接写入伪控制台。当前渲染器保留普通文本和换行，并过滤常见 ANSI 控制序列；它不是完整的 VT 高级渲染器，因此复杂颜色、光标定位和全屏 TUI 程序不会完全复现。
