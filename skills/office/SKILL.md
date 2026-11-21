---
name: office
description: 通过把 python-docx、openpyxl 或 python-pptx 当外部程序运行，来创建、读取和编辑 .docx、.xlsx、.pptx。任何文档任务开始前先加载。
---

# Office 文档

本宿主不内置文档解析器，也不内置写入器。你产出的每一个 `.docx`、`.xlsx`、`.pptx`，都是
**你**用 shell 工具跑起来的一个外部 Python 程序写出来的。没有兜底方案：手写 OOXML zip 就是
一个坏文件，而一个能打开的空白文档比一句诚实的拒绝更糟。

## 1. 写任何东西之前，先检查工具链

用 shell 工具跑：

    hearth doctor

`hearth` 可能不在 `PATH` 上，因为宿主是用绝对路径启动的。如果 shell 回一句
`hearth: not recognized`，就用完整路径跑同一个二进制，或者在源码树里跑
`cargo run -q -p hearth-cli -- doctor`（包名是 `hearth-cli`；它构建出的二进制叫 `hearth`）。
不要跳过这次探测，也不要假定某个库已经装好：**如果探测根本跑不起来，就说它没跑起来，
然后停下。**

读 `office` 那一行。它对 `docx`、`xlsx`、`pptx`、`convert` 逐项报告 `missing`、
`unknown (...)`，或者那个能 import 该库的解释器：

    OK    office  docx=python 3.12 xlsx=python 3.12 pptx=missing convert=missing

状态词可能是 `OK` 或 `WARN`；`WARN` 仍是一个答案，不是失败。它下面若有一行 `->`，那句话
就是在说这台机器上什么不可用。

如果你被要求的那个格式**没有**被报告为可用——是 `missing`、`unknown`，或整行里根本没有它
——那么：

* **停。** 不要创建那个文件。不要创建一个「以后再填」的空壳或占位文件。不要手写 XML 或
  zip。
* 报告哪个格式不可用、对应的 Python 库没装，以及装它的命令：
  `python -m pip install python-docx`（`.docx`）、
  `python -m pip install openpyxl`（`.xlsx`）、
  `python -m pip install python-pptx`（`.pptx`）。
* 明说你没做什么。一份被拒绝的文档花掉一条消息；一份悄悄空掉的文档花掉一个工作日。

## 2. 写脚本，然后运行它

不要把多行程序粘进一条 shell 字符串：`pwsh` 与 POSIX shell 的引号规则不一样。用文件工具写
一个 `.py`，跑它，然后如果用户不要就删掉它。

### 创建 .docx

```python
from docx import Document

document = Document()
document.add_heading("季度报告", level=1)
document.add_paragraph("正文第一行。")
document.add_paragraph("第二行。", style="List Bullet")
table = document.add_table(rows=2, cols=2)
table.cell(0, 0).text = "region"
table.cell(0, 1).text = "total"
document.save("out.docx")
```

### 创建 .xlsx

```python
from openpyxl import Workbook

book = Workbook()
sheet = book.active
sheet.title = "Summary"
sheet.append(["region", "total"])
sheet.append(["north", 1250])
sheet["B2"].number_format = "#,##0"  # 格式是单元格属性，不是值的一部分
sheet["D1"] = "=SUM(B2:B3)"          # 公式是文本，绝不预先算好
book.save("out.xlsx")
```

数字格式从来不会从值里推导出来：写 `1250` 会让单元格停在 `General`，Excel 不会显示千位
分隔符，所以要为你关心的每个单元格设 `number_format`（`#,##0`、`0.00`、`yyyy-mm-dd`、
`0%`）。

### 创建 .pptx

```python
from pptx import Presentation

deck = Presentation()                # 空白演示文稿没有幻灯片：要自己加
slide = deck.slides.add_slide(deck.slide_layouts[1])   # 1 = "Title and Content"
slide.shapes.title.text = "状态"
slide.placeholders[1].text = "一条要点\n另一条要点"
second = deck.slides.add_slide(deck.slide_layouts[1])
second.shapes.title.text = "下一步"
second.placeholders[1].text = "发布\n观察"
deck.save("out.pptx")
```

`.text` 里的 `\n` 开一个新段落；`\v` 是同一段落内的软换行。项目符号本身继承自母版的正文
样式，所以不要手写 `buChar`。版式与占位符索引属于模板：`slide_layouts[1]` 只在默认模板里
是 "Title and Content"，正文在那里是 `placeholders[1]`。版式变了的时候，打印
`[(p.placeholder_format.idx, p.name) for p in slide.placeholders]` 而不是靠猜。

### 读取文档

```python
from docx import Document

for paragraph in Document("in.docx").paragraphs:
    if paragraph.text.strip():
        print(paragraph.text)
```

表格用 `openpyxl.load_workbook("in.xlsx").active.iter_rows(values_only=True)` 迭代；演示
文稿用 `pptx.Presentation("in.pptx").slides`。如果答案需要的是版面保真而不是数值，那就改为
要求一次转换。

### 转换（PDF、ODT、旧格式）

只在 `hearth doctor` 报告 `convert` 可用时才做：

    soffice --headless --convert-to pdf --outdir . in.docx

当它报告 `convert=missing` 时，答案是一次明确的拒绝：说这台机器上没装 LibreOffice
（`soffice`）、它是一个独立应用而不是 `python -m pip` 能装的东西、转换出来的文件没有生成。
绝不写一个空的或 0 字节的 `.pdf`，也绝不用一个只渲染了部分文档的库来顶替。

LibreOffice 是转换器，不是生成器：用它改格式，永远不要用它造内容。

## 3. 报告成功之前先验证

每个脚本最后都要重新打开自己的输出并打印一个计数：

```python
from docx import Document
print("paragraphs:", len([p for p in Document("out.docx").paragraphs if p.text.strip()]))
```

另外两种格式，要把**你被要求做出来的东西**读回来，而不只是读那个文件：

```python
from openpyxl import load_workbook

sheet = load_workbook("out.xlsx").active
print("rows:", sheet.max_row, "D1:", repr(sheet["D1"].value),
      "B2 format:", sheet["B2"].number_format)
```

```python
from pptx import Presentation

deck = Presentation("out.pptx")
print("slides:", len(deck.slides),
      "titles": [slide.shapes.title.text for slide in deck.slides])
```

`load_workbook` 返回的是公式的**字符串**（`'=SUM(B2:B3)'`），因为 `openpyxl` 不重算；那是
预期值，不是 bug。如果控制台代码页不是 UTF-8，打印一个 `•` 会抛
`UnicodeEncodeError: 'gbk' codec can't encode character '\u2022'` —— 那是控制台的问题，
不是文档的问题。打印计数和 ASCII，或者在脚本开头写 `import sys` 与
`sys.stdout.reconfigure(encoding="utf-8")`。

如果计数是零，这个文件就不是交付物——要说出来，而不是把它报成已完成。说清你写了哪个路径、
量到了哪些计数。

## 规矩

* 一份文档一个脚本，写进工作区，跑一次。不做 GUI 自动化，也绝不让 LibreOffice 不带
  `--headless` 留着跑。
* 除非那条命令真的跑了，否则绝不说某份文档被创建、被转换或被验证过；也绝不报告一个你没
  打印出来的计数。
* 这些片段里的 `python` 指的是 `hearth doctor` 点名的那个解释器。如果机器上只有 `py -3`，
  就用 `py -3`；只有 `python3`，就用 `python3`。