---
name: Bug report
about: Something that is not a download from a site that works
labels: bug
---

## What happened

<!-- The exact command, and the exact output. `-v` adds the request log. -->

```console
$ tafas ...

```

## What you expected

## Environment

<!-- `tafas --capabilities` is the most useful single line: it names the
     version, the extractor routes and the build's limits. -->

```console
$ tafas --capabilities

$ tafas --version
```

- OS and architecture:
- How it was installed (release archive, package, cargo, other):

## If a file was wrong

- Was the destination file created at all? A verification failure leaves no
  file and keeps a `.part`.
- Does the file play in a player? Which one, and what did it say?
- If a remux was involved, `ls -l` of the `.part` files is useful.

Do not attach copyrighted media unless you own it or it is your own upload.
