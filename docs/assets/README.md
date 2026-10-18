# README 视觉资源

- `task-flow.gif`：原创任务流程动画，1200 × 370，循环时长 2.56 秒；GIF 编码会合并相同帧。
- `task-flow.png`：同一流程的静态版本，供不希望观看动画的读者使用。
- `render-flow.py`：使用 Pillow 绘制的生成脚本，无远程图片依赖。不是运行状态可视化。

重新生成：安装 Pillow，确保系统存在 DejaVu Sans Mono 字体（路径见脚本），在仓库根目录执行：

```bash
python3 docs/assets/render-flow.py
```

修改流程节点时应同步 README 的图片替代文本。图中文字使用英文以兼顾字体可复现性，README 替代文本提供中文说明。

## 架构图

`architecture.png` 为直接嵌入的静态架构图，不依赖 Mermaid 渲染。使用 Pillow 与 Noto Sans CJK 字体重新生成：

```bash
python3 docs/assets/render-architecture.py
```

## 项目封面

`cover.png` 为原创文字与几何图形封面（1600 × 600）。重新生成需 Pillow、Noto Sans CJK 与 DejaVu Sans Bold：

```bash
python3 docs/assets/render-cover.py
```
