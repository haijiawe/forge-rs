# Office 技能，端到端跑一遍

日期：2026-11-20。主机：Windows，MSVC 构建，`python` = 3.14.5
（`C:\Users\Administrator\AppData\Local\Python\pythoncore-3.14-64\python.exe`）。

这是 `skills/office/SKILL.md` 的验证记录。它背后的探针（`hearth doctor`、`crates/cli/src/office.rs`）早就知道是好的；从没被验证过的，是**那些指令本身**能不能产出一份真正的文档。答案是五处不能，而这五处现在都修了。下面每一条都在本机跑过，没有一条是从代码里推的。

## 1. 探针

```powershell
& 'D:\hearth-target\hearth\debug\hearth.exe' doctor
```

```
WARN  office     docx=python 3.14 xlsx=python 3.14 pptx=python 3.14 convert=missing
                 -> unavailable on this machine: format conversion (LibreOffice soffice). Install LibreOffice for format conversion; say so instead of writing an empty file.
```

那一行是 `WARN` 而不是 `OK`，后面跟着一句 `->` 说明。三种格式的原生写出都可用；格式转换不可用，因为这台机器上没有 LibreOffice。对这台机器来说，这就是正确的结论。

## 2. 严格照着发出去的技能走

临时目录 `D:\hearth-target\office-check\`。按 SKILL.md §2，每份文档各自一个 `.py` 文件，用 file 工具写出来，用 doctor 点名的解释器（`python`）跑。脚本逐字照抄 §2，只在 docx 那个后面追加了 §3 的读回。

```powershell
cd 'D:\hearth-target\office-check'
python make_docx.py    # paragraphs: 3
python make_xlsx.py    # (no output: the snippet prints nothing)
python make_pptx.py    # (no output: the snippet prints nothing)
```

出现了三个文件：`out.docx` 35,488 B、`out.xlsx` 4,900 B、`out.pptx` 28,356 B。它们能打开。短板在它们的内容里。

```powershell
python verify.py
```

```
== out.docx
  zip ok, members: 17
  [Content_Types].xml: True
  word/document.xml: True
docx read-back paragraphs: ['Quarterly report', 'First line of the body.', 'Second line.']
docx read-back table cells: [['region', 'total'], ['', '']]
docx read-back styles: ['Heading 1', 'Normal', 'List Bullet']

== out.xlsx
  xl/workbook.xml: True
xlsx sheet name in workbook.xml: True
xlsx formula in sheet1.xml: True
xlsx read-back rows: [('region', 'total', None, '=SUM(B2:B3)'), ('north', 1250, None, None)]
xlsx read-back B2 number_format: 'General'          <-- no number format

== out.pptx
pptx read-back slide count: 1                       <-- one slide, not two
  slide 1 title='Status' texts=['Status', 'One bullet', 'Another bullet']
```

### 错的那五处

1. **§1 根本跑不起来。** 本机的 `hearth` 不在 `PATH` 上；照抄的那条命令以
   `hearth: 术语 'hearth' 不会被识别为 cmdlet、函数、脚本文件或可执行程序的名称。`
   失败，而技能没给任何退路，所以照着做的模型拿不到那段它被告知要读的输出行。
2. **§1 没有描述它会得到的答案。** 示例给的是一行 `OK`；真实输出是 `WARN` 加一行 `->` 续写。一个专门找 `OK` 的模型，会把 `WARN` 读成探针失败。
3. **§2 的 `.xlsx` 少了设数字格式这一步。** `sheet["B2"]` 读回来是 `General`。设值从来不设格式，所以照发出去的片段根本够不到格式。
4. **§2 的 `.pptx` 只造了一页。** 两页的稿子需要第二次 `add_slide`；片段里没提，也没说 `.text` 里的 `\n` 到底出项目符号还是出长长一行。
5. **§Convert 说了什么时候不该转，但没说要告诉用户什么。** 在 `convert=missing` 时，模型没有拒绝的话术，所以那句诚实的话（"没有装 LibreOffice"）哪儿都没写下来。

## 3. SKILL.md 改了什么

`+58 −1`，四处修改加上验证示例（完整 diff 在工作树里）。没有任何行为规则被放松；§1 的 STOP 规则、§3 的"零字节不算交付物"、以及"永远不要声称你跑过没跑的东西"，都没动。

* §1：`hearth` 不在 `PATH` 上时怎么跑探针
  （`cargo run -q -p hearth-cli -- doctor`——下面验证过），以及 `WARN` 也是一个答案。
* §2 `.xlsx`：`sheet["B2"].number_format = "#,##0"`，以及为什么只设值会留下 `General`。
* §2 `.pptx`：第二页、`\n` 与 `\v` 的区别、从母版继承的项目符号，以及布局/占位符索引是模板事实、要打印出来而不是猜。
* §Convert：`convert=missing` 时该给的拒绝——点名 LibreOffice、说明 `python -m pip` 装不了它、永远不要写一个 0 字节的 `.pdf`。
* §3：`.xlsx` 和 `.pptx` 的读回片段、`openpyxl` 返回公式字符串这个事实，以及打印 `•` 时的 cp936 控制台陷阱。

## 4. 重跑修好的指令

打补丁后 §2 的每个片段都被写成脚本、原样跑了一遍。

```powershell
python fixed_docx.py   # paragraphs: 3
python fixed_xlsx.py
python fixed_pptx.py
python verify_fixed.py
```

```
### PART 1 - OOXML structure
== fixed.docx
   zip integrity (testzip): ok
   members: 17
   [Content_Types].xml present: True
   required part: word/document.xml -> word/document.xml
== fixed.xlsx
   zip integrity (testzip): ok
   members: 9
   [Content_Types].xml present: True
   required part: xl/workbook.xml -> xl/workbook.xml
== fixed.pptx
   zip integrity (testzip): ok
   members: 40
   [Content_Types].xml present: True
   required part: ppt/presentation.xml -> ppt/presentation.xml

### PART 2 - requested content is inside the XML
docx heading 'Quarterly report' in xml: True
docx paragraph 'First line of the body.' in xml: True
docx table element <w:tbl> in xml: True
docx table cells 'region'/'total' in xml: True
xlsx header 'region'/'total' in sheet1.xml: True
xlsx formula string 'SUM(B2:B3)' in sheet1.xml: True
xlsx numberFormat applied -> raw 'numFmtId' on B2: ['<c r="B2" s="1" t="n">']
pptx slide ids in presentation.xml: 2
pptx slide parts: ['ppt/slides/slide1.xml', 'ppt/slides/slide2.xml']
pptx slide1 has 'Status' + 'One bullet': True
pptx slide2 has 'Next steps' + 'Ship it': True
pptx bullets inherited from master (buChar count): 9

### PART 3 - read back with each file's own library
docx paragraphs: 3
docx text: ['Quarterly report', 'First line of the body.', 'Second line.']
docx styles: ['Heading 1', 'Normal', 'List Bullet']
docx table rows x cols: 2 x 2
docx table cells: [['region', 'total'], ['north', '1250']]
xlsx sheet title: Summary
xlsx rows: 2 cols: 4
xlsx values: [('region', 'total', None, '=SUM(B2:B3)'), ('north', 1250, None, None)]
xlsx D1 raw: '=SUM(B2:B3)'
xlsx B2 value: 1250 number_format: '#,##0'
xlsx A1/B1 header: 'region' 'total'
pptx slides: 2
   slide 1: title='Status' body_paragraphs=['One bullet', 'Another bullet']
   slide 2: title='Next steps' body_paragraphs=['Ship it', 'Watch it']
```

`.xlsx` 的数字格式在部件 XML 里看得见：`B2` 上带 `s="1"`，而
`xl/styles.xml` 把单元格样式 1 解析到 `numFmtId="3"`——也就是内置的 `#,##0` 格式，所以没有写任何自定义 `<numFmt>` 条目。读回在语义上确认了它：`number_format == '#,##0'`。

写进技能里的两条主张是测出来的，不是假定的：

```powershell
python -c "from pptx import Presentation; d=Presentation(); s=d.slides.add_slide(d.slide_layouts[1]); s.shapes.title.text='T'; s.placeholders[1].text='A\vB'; print(len(s.placeholders[1].text_frame.paragraphs), [r.text for r in s.placeholders[1].text_frame.paragraphs[0].runs], '<a:br' in s.placeholders[1].text_frame._txBody.xml, [(p.placeholder_format.idx, p.name) for p in s.placeholders])"
```

```
1 ['A', 'B'] True [(0, 'Title 1'), (1, 'Content Placeholder 2')]
```

一个段落、两个 run、一个 `<a:br/>`——`\v` 是软换行；而技能现在推荐的占位符索引打印也确实能用。

探针的退路也是真的——打补丁后 §1 给的那条原命令：

```powershell
$env:CARGO_TARGET_DIR='D:\hearth-target\hearth'
cargo run -q -p hearth-cli -- doctor
```

```
WARN  office     docx=python 3.14 xlsx=python 3.14 pptx=python 3.14 convert=missing
                 -> unavailable on this machine: format conversion (LibreOffice soffice). Install LibreOffice for format conversion; say so instead of writing an empty file.
```

（是 `hearth-cli` 不是 `hearth`：这个修复的早期草稿写的是 `-p hearth`，cargo 拒绝了它——原样照抄，反引号也照 cargo 打出来的样子留着：

```
error: package(s) `hearth` not found in workspace `D:\魔改版Agent框架\hearth`
```

退出码 101。技能文本里带的是验证过的包名。）

## 5. 拒绝的路径

这台机器转不了，所以对"给我一个 PDF"的诚实答案就是拒绝。验证过技能是这么说的，而且什么都没被写出来：

```
soffice found on PATH: False
exists C:\Program Files\LibreOffice\program\soffice.exe -> False
exists C:\Program Files (x86)\LibreOffice\program\soffice.exe -> False
pdf files in scratch dir: []
zero-byte files in scratch dir: []
```

模型如果无视规则，会跑的那条天真命令：

```powershell
soffice --headless --convert-to pdf --outdir . out.docx
```

```
soffice: 术语 'soffice' 不会被识别为 cmdlet、函数、脚本文件或可执行程序的名称。
请检查名称的拼写或验证路径是否正确(如果包含路径)，然后重试。
```

没有 `.pdf`，也没有 0 字节文件被产出。照着打补丁后的 §Convert 做的模型，会用话回答：

> 我出不了这个 PDF：这台机器上没有装 LibreOffice（`soffice`），所以
> `hearth doctor` 报 `convert=missing`。PDF 转换是 LibreOffice 干的活，
> 那是一个独立应用，`python -m pip` 装不了它。`quarterly-report.pdf`
> **没有**被创建。交付物是
> `D:\hearth-target\office-check\fixed.docx`；需要 PDF 的话请装 LibreOffice。

## 6. 结论

这个技能**修完之后是对的**，**修之前是不完整的**。库没问题：
`python-docx` 1.2.0、`openpyxl` 3.1.5、`python-pptx` 1.0.2 都写出了各自的产物，也都重新打开了自己写出的产物。毛病在指令里——一个定位不到的探针、一种它没描述的答案形状（`WARN`）、缺一行设数字格式、一个只要一页而实际要两页的示例、以及一句没有话术的拒绝。

这里**没有**验证的：

* 没有任何文档在 Word、Excel 或 PowerPoint 里打开过；本机也没有 LibreOffice 能渲染一份。"它是一个合法的 OOXML 包，它自己的库能重新打开，且其中 XML 含有被要求的文字"——这是量到的东西；不是"它在屏幕上看起来对"。
* `.pptx` 的项目符号是从幻灯片母版的正文样式继承来的（`slideMaster1.xml` 里 9 个 `buChar`，`•`）。格式就是这么工作的，但项目符号的渲染没被看到过。
* `=SUM(B2:B3)` 存的是文本，且 `B3` 是空的，所以没有任何东西去计算它。公式在场、重读出来是 `'=SUM(B2:B3)'`；没有做重算，因为 openpyxl 不做这个。
* 在**装了** LibreOffice 的机器上跑 `hearth doctor`、以及 §1 的 `OK` 分支，都没被跑到：这台机器上 `convert` 是缺的。

## 文件

| 路径 | 是什么 |
| --- | --- |
| `skills/office/SKILL.md` | 打过补丁，+58 −1 |
| `docs/office-run.md` | 这份记录 |
| `D:\hearth-target\office-check\make_*.py` | 三个照抄的脚本 |
| `D:\hearth-target\office-check\fixed_*.py` | 三个来自打补丁后技能的脚本 |
| `D:\hearth-target\office-check\verify.py`、`verify_fixed.py` | zip + XML + 读回检查 |
| `D:\hearth-target\office-check\refusal_check.py` | 转换拒绝的探针 |