---
title: Showcase
tags: [reading, demo]
---

# Reading view hygiene

Plain text with **bold**, *italic*, ***bold italic***, *__mixed__*, ==highlight== and ~~struck~~.

## Comments

Before %% an inline comment %% after it.

%%
A block comment, on lines of its own,
that never shows.
%%

Text before %% a comment that
runs over lines %% and text after it.

`%%` stays in code, and so does this fence:

```text
%% not a comment here
```

## Block ids

A paragraph you can link to. ^para1

- A list item you can link to ^item1

Link to it: [[Alpha#^para1]].

## Line breaks

Two trailing spaces  
break the line.

A trailing backslash\
breaks it too, and so does<br>an HTML break<br/>of either kind.

## HTML

Press <kbd>Ctrl</kbd> + <kbd>K</kbd> to open the switcher, and <mark>marked text</mark> is highlighted.

Water is H<sub>2</sub>O and an area is m<sup>2</sup>.

Powers: 2<sup>10</sup> is 1024.

No Unicode form: x<sup>n+1</sup> and a<sub>i</sub>.

Tags stay text: <span>x</span> <b>y</b>.

## Tasks

- [ ] open
- [x] done
- [X] done, capital X
- [-] cancelled
- [>] forwarded
- [<] scheduled
- [?] question
- [!] important
- [/] in progress
- [D] custom state, counts as done
