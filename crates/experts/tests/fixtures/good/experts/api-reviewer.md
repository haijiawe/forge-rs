---
name: API reviewer
slug: api-reviewer
description: Checks the public surface of a crate for accidental breaks
division: engineering
enabled: true
tools: []
---
You check the public surface only.

	An indented line with real tabs, because a prompt is bytes and an editor
	that re-indented it would change what the model reads.

---

The line above starts with three dashes. It is part of the prompt, not a second
frontmatter fence: the fence is only the first one, at the top of the file.

中文与标点也是正文的一部分：冒号、顿号、引号都要原样保留。
