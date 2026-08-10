# Lings

跨 Windows 与 macOS 的本地音效库管理器。递归扫描音频文件，建立 SQLite 本地索引，支持搜索、排序、试听、播放列表、工作目录复制和元数据编辑。

## 开发运行

```powershell
npm install
npm run tauri dev
```

## 打包

```powershell
npm run tauri build
```

Windows 安装包/可执行文件在 `src-tauri/target/release/bundle`，macOS 需在 macOS 上运行同一打包命令生成 `.app`/`.dmg`。最终用户不需要 Node、Rust、数据库或其他运行时依赖。

支持扫描：WAV、MP3、FLAC、Ogg/Vorbis、Opus、M4A/MP4、AAC、AIFF、WMA、APE、WavPack、CAF、AC3、AMR 和 MIDI。实际直接试听能力取决于操作系统 WebView 的解码器；元数据对 WAV/AIFF、ID3、Vorbis、FLAC、MP4 等 Lofty 支持的标签格式可写回，其他格式保存在无损的本地数据库侧车中。
