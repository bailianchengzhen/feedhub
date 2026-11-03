# yt.solver.*.min.js 的来源与许可

这两个文件是 **yt-dlp/ejs 0.8.0** 的发布产物，原样放进仓库（不是运行时下载）：

- 上游：https://github.com/yt-dlp/ejs ，release `0.8.0`
  - `https://github.com/yt-dlp/ejs/releases/download/0.8.0/yt.solver.core.min.js`
  - `https://github.com/yt-dlp/ejs/releases/download/0.8.0/yt.solver.lib.min.js`
- 许可：**Unlicense**（两个文件的头部都写着 `SPDX-License-Identifier: Unlicense`），可自由嵌入。
  `yt.solver.lib.min.js` 里打进去的两个依赖：meriyah 6.1.4（MIT）、astring 1.9.0（MIT）。
- 版本怎么确认的：yt-dlp 把版本和哈希钉在
  `yt_dlp/extractor/youtube/jsc/_builtin/vendor/_info.py`（`VERSION = '0.8.0'`，哈希是 **sha3-512**）。
  下载后逐个核对：

  | 文件 | 大小 | sha3-512（与 yt-dlp 钉的一致） |
  |---|---|---|
  | `yt.solver.core.min.js` | 6945 | `ee5b307d…0ab599` |
  | `yt.solver.lib.min.js` | 151561 | `8420c259…2ad053` |

- 怎么用：`feedhub.source.youtube.YoutubeEjs` 在 QuickJS（quickjs-kt，Apache-2.0）里先跑 lib、
  再 `Object.assign(globalThis, lib)`、再跑 core，然后调 `jsc({type:'player', player:<播放器 JS>, …})`。
  调用形状照 yt-dlp 自己的 `ejs.py::_construct_stdin`。
- **不要在运行时从网上下载可执行 JS**：这两个脚本随包走；只有"播放器脚本"本身
  （`player_ias.vflset/…/base.js`，YouTube 自己的产物，随时在变）才在运行时取一次并缓存。
