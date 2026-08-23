# Curosu

Windows 上的 osu! lazer 风格光标覆盖层。

Curosu 基于 [OsuCursirWin](https://github.com/xyc-233/OsuCursirWin)，使用 Rust 重写，复刻 osu! lazer 的光标动画、缩放、发光与旋转效果。release 包约 **3.5 MB**。

> 非 osu! 官方项目，与 osu! 或 ppy 无关。

## 演示

[![Curosu 演示](https://i2.hdslb.com/bfs/archive/46fb84a45c885bd5553250883b43ed04e3d07dd7.jpg)](https://www.bilibili.com/video/BV1Cvbr6hEvd)

点击上方封面在 B 站观看演示视频。

## 特性

- 自绘 osu! 风格光标覆盖层，隐藏 Windows 系统光标
- 按下时缩放并发光，拖动时跟随鼠标旋转
- 释放时先快速旋转 3 圈，再以弹性动画回正
- 支持敲击音效、悬停音效、音量调节和窗口拉伸提示
- 设置窗口支持调整光标大小与开机自启
- 托盘图标、点击穿透、置顶和多显示器 DPI 自适应
- 鼠标钩子失效时自动回退到位置轮询

## 下载与安装

从 [Releases](https://github.com/yaki1210/CurOsu/releases/latest) 下载 `curosu-win-x64.zip` 并解压，双击 `install.bat`（弹出 UAC 时点"是"）。脚本会把 `curosu.exe` 安装到 `C:\Program Files\Curosu` 并用本机自签名证书签名，之后从该目录启动即可（可创建快捷方式）。

> **为什么要运行安装脚本？** 程序清单请求了 `uiAccess="true"`，让光标能覆盖开始菜单、通知中心等系统窗口。Windows 要求这类程序必须**已签名**且位于**安全目录**（如 Program Files），否则启动会报错"从服务器返回了一个参照"（错误 8235）。安装脚本会在本机生成仅本机信任的自签名证书并完成签名，无需购买代码签名证书；换电脑后需重新运行一次脚本。

程序首次启动会打开设置窗口；关闭设置窗口只会将程序隐藏到托盘，不会退出。托盘图标操作如下：

| 操作 | 行为 |
| --- | --- |
| 左键单击 | 打开或聚焦设置窗口 |
| 右键单击 | 打开设置、关闭光标或退出程序 |

Curosu 仅支持 Windows。部分独占全屏程序可能不会显示覆盖层，建议使用无边框或窗口化模式。

## 从源码构建

需要 Windows 和 Rust stable 工具链：

```powershell
cargo build --release
```

构建产物位于：

```text
target/release/curosu.exe
```

GitHub Actions 会在推送到 `main` 时构建 artifact，并在推送 `v*` tag 时自动创建 Release。

## 配置

配置文件位于 `%APPDATA%\Curosu\settings.json`：

| 字段 | 含义 | 默认值 |
| --- | --- | --- |
| `cursor_width` | 光标宽度（16–64 px） | `30` |
| `tap_sound_enabled` | 敲击音效 | `true` |
| `tap_sound_volume` | 敲击音量（0–1） | `1.0` |
| `hover_sound_enabled` | 悬停音效 | `true` |
| `hover_sound_volume` | 悬停音量（0–1） | `1.0` |
| `hover_sound_as_resize_prompt` | 窗口拉伸时播放悬停音效 | `false` |
| `auto_start` | 开机自启 | `false` |
| `fullscreen_overlay_exclusion_executables` | 设置窗口中选择的全屏例外程序 | 空 |

在“全屏例外”中刷新当前窗口列表，选择程序后点击“添加”。该程序全屏时会继续显示动画光标。

## 技术实现

- Rust + Win32 API 实现透明分层窗口与全局光标覆盖
- 通过 `WH_MOUSE_LL` 捕获鼠标按下、移动和释放事件
- 使用 PNG 纹理进行双线性缩放、旋转和高光合成
- 使用 `eframe/egui` 实现设置窗口
- 使用 `UpdateLayeredWindow` 呈现预乘 ARGB 帧

## 许可

[MIT License](LICENSE)
