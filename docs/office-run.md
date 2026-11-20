# The Office skill, run end to end

Date: 2026-11-20. Host: Windows, MSVC build, `python` = 3.14.5
(`C:\Users\Administrator\AppData\Local\Python\pythoncore-3.14-64\python.exe`).

This is the verification record for `skills/office/SKILL.md`. The probe behind it
(`hearth doctor`, `crates/cli/src/office.rs`) was already known to work; what had
never been tested is whether **the instructions themselves** produce a real
document. They did not, in five places, which are now fixed. Everything below was
run on this host; nothing here is inferred from the code.

## 1. The probe

```powershell
& 'D:\hearth-target\hearth\debug\hearth.exe' doctor
```

```
WARN  office     docx=python 3.14 xlsx=python 3.14 pptx=python 3.14 convert=missing
                 -> unavailable on this machine: format conversion (LibreOffice soffice). Install LibreOffice for format conversion; say so instead of writing an empty file.
```

The line is `WARN`, not `OK`, and it is followed by a `->` sentence. Native
writing is available for all three formats; format conversion is not, because
LibreOffice is absent. That is the correct verdict for this machine.

## 2. Following the shipped skill literally

Scratch directory `D:\hearth-target\office-check\`. Per SKILL.md §2 each
document gets its own `.py` file, written with the file tool and run with the
interpreter the doctor named (`python`). The scripts were copied from §2
verbatim, with the §3 read-back appended to the docx one.

```powershell
cd 'D:\hearth-target\office-check'
python make_docx.py    # paragraphs: 3
python make_xlsx.py    # (no output: the snippet prints nothing)
python make_pptx.py    # (no output: the snippet prints nothing)
```

Three files appeared: `out.docx` 35,488 B, `out.xlsx` 4,900 B, `out.pptx` 28,356 B.
They open. What they contain is where the skill fell short.

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

### The five things that were wrong

1. **§1 could not run.** `hearth` is not on `PATH` on this host; the literal
   command fails with `hearth: 术语 'hearth' 不会被识别为 cmdlet、函数、脚本文件或可执行程序的名称。`
   and the skill offered no fallback, so a model following it could not obtain
   the line it is told to read.
2. **§1 did not describe the answer it would get.** The example shows a single
   `OK` line; the real output is `WARN` plus a `->` continuation. A model
   scanning for `OK` can read `WARN` as a failed probe.
3. **§2 `.xlsx` had no number-format step.** `sheet["B2"]` came back `General`.
   Setting a value never sets a format, so the format was unreachable from the
   shipped snippet.
4. **§2 `.pptx` built one slide.** A two-slide deck needs a second `add_slide`;
   nothing in the snippet said so, and nothing said whether `\n` in `.text`
   yields bullets or one long line.
5. **§Convert said when not to convert, never what to tell the user.** With
   `convert=missing` a model had no refusal wording, so the honest answer
   ("LibreOffice is not installed") was not written down anywhere.

## 3. What changed in SKILL.md

`+58 −1`, four edits plus the verification examples (the full diff is in the
working tree). No behavioural rule was weakened; §1's STOP rule, §3's
"zero means not a deliverable", and the "never claim what you did not run" rule
are untouched.

* §1: how to run the probe when `hearth` is not on `PATH`
  (`cargo run -q -p hearth-cli -- doctor` — verified below), and that `WARN` is
  still an answer.
* §2 `.xlsx`: `sheet["B2"].number_format = "#,##0"` and why the value alone
  leaves `General`.
* §2 `.pptx`: a second slide, `\n` vs `\v`, bullets inherited from the master,
  and that layout/placeholder indices are template facts to print, not guess.
* §Convert: the refusal to give when `convert=missing` — name LibreOffice, say
  `python -m pip` cannot install it, never write a 0-byte `.pdf`.
* §3: read-back snippets for `.xlsx` and `.pptx`, the fact that `openpyxl`
  returns the formula string, and the cp936 console trap when printing `•`.

## 4. Re-running the fixed instructions

Each snippet from the patched §2 was written to a script and run, unchanged.

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

The `.xlsx` number format is visible in the part XML as `s="1"` on `B2`, and
`xl/styles.xml` resolves cell style 1 to `numFmtId="3"` — the built-in `#,##0`
format, so no custom `<numFmt>` entry is written. Read-back confirms it
semantically as `number_format == '#,##0'`.

Two claims added to the skill were tested rather than assumed:

```powershell
python -c "from pptx import Presentation; d=Presentation(); s=d.slides.add_slide(d.slide_layouts[1]); s.shapes.title.text='T'; s.placeholders[1].text='A\vB'; print(len(s.placeholders[1].text_frame.paragraphs), [r.text for r in s.placeholders[1].text_frame.paragraphs[0].runs], '<a:br' in s.placeholders[1].text_frame._txBody.xml, [(p.placeholder_format.idx, p.name) for p in s.placeholders])"
```

```
1 ['A', 'B'] True [(0, 'Title 1'), (1, 'Content Placeholder 2')]
```

One paragraph, two runs, an `<a:br/>` — `\v` is a soft break; and the
placeholder-index print the skill now recommends works.

The probe fallback is real too — the exact command the patched §1 gives:

```powershell
$env:CARGO_TARGET_DIR='D:\hearth-target\hearth'
cargo run -q -p hearth-cli -- doctor
```

```
WARN  office     docx=python 3.14 xlsx=python 3.14 pptx=python 3.14 convert=missing
                 -> unavailable on this machine: format conversion (LibreOffice soffice). Install LibreOffice for format conversion; say so instead of writing an empty file.
```

(`hearth-cli`, not `hearth`: an earlier draft of the fix said `-p hearth`, and cargo
refused it — verbatim, with backticks as cargo printed them:

```
error: package(s) `hearth` not found in workspace `D:\魔改版Agent框架\hearth`
```

exit 101. The skill text carries the verified package name.)

## 5. The refusal path

The machine cannot convert, so the honest answer to "give me a PDF" is a refusal.
Verified that the skill says so and that nothing gets written:

```
soffice found on PATH: False
exists C:\Program Files\LibreOffice\program\soffice.exe -> False
exists C:\Program Files (x86)\LibreOffice\program\soffice.exe -> False
pdf files in scratch dir: []
zero-byte files in scratch dir: []
```

The naive command a model would run if it ignored the rule:

```powershell
soffice --headless --convert-to pdf --outdir . out.docx
```

```
soffice: 术语 'soffice' 不会被识别为 cmdlet、函数、脚本文件或可执行程序的名称。
请检查名称的拼写或验证路径是否正确(如果包含路径)，然后重试。
```

No `.pdf` and no 0-byte file was produced. A model following the patched §Convert
answers, in words:

> I cannot produce the PDF: LibreOffice (`soffice`) is not installed on this
> machine, so `hearth doctor` reports `convert=missing`. PDF conversion is done by
> LibreOffice, which is a separate application and cannot be installed with
> `python -m pip`. `quarterly-report.pdf` was **not** created. The `.docx` at
> `D:\hearth-target\office-check\fixed.docx` is the deliverable; install
> LibreOffice if a PDF is needed.

## 6. Verdict

The skill is **correct after the fix**, and was **incomplete before it**. The
libraries are not broken: `python-docx` 1.2.0, `openpyxl` 3.1.5 and
`python-pptx` 1.0.2 all wrote, and all re-opened, their own output. The defects
were in the instructions — a probe that could not be located, an answer shape
(`WARN`) it did not describe, a missing number-format line, a one-slide example
where two slides were asked for, and a refusal with no wording.

What is **not** verified here:

* No document was opened in Word, Excel or PowerPoint; there is no LibreOffice
  on this host to render one either. "It is a valid OOXML package that its own
  library re-opens and whose XML contains the requested text" is what was
  measured — not "it looks right on screen".
* The `.pptx` bullets are inherited from the slide master's body style (9
  `buChar` in `slideMaster1.xml`, `•`). That is how the format works, but the
  bullet was not seen rendered.
* `=SUM(B2:B3)` is stored as text and `B3` is empty, so nothing computed it. The
  formula is present and re-reads as `'=SUM(B2:B3)'`; no recalculation was
  performed, since openpyxl does not do that.
* `hearth doctor` on a machine that *has* LibreOffice, and the `OK` branch of §1,
  were not exercised: `convert` is missing here.

## Files

| Path | What |
| --- | --- |
| `skills/office/SKILL.md` | patched, +58 −1 |
| `docs/office-run.md` | this transcript |
| `D:\hearth-target\office-check\make_*.py` | the three literal scripts |
| `D:\hearth-target\office-check\fixed_*.py` | the three scripts from the patched skill |
| `D:\hearth-target\office-check\verify.py`, `verify_fixed.py` | zip + XML + read-back checks |
| `D:\hearth-target\office-check\refusal_check.py` | the conversion refusal probe |