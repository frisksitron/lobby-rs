# Figtree

Static Figtree 2.002 Regular, Medium, SemiBold and Bold, cut from the variable font
`ofl/figtree/Figtree[wght].ttf` of google/fonts at commit
`7888febb355d9e2c2c4aa357d20734e383f2198f`
(<https://github.com/google/fonts/tree/7888febb355d9e2c2c4aa357d20734e383f2198f/ofl/figtree>).
Licensed under the SIL Open Font License 1.1: see `OFL.txt`, which is that folder's `OFL.txt`.
The fonts are embedded in the app (`src/fonts.rs`).

The four weights are the ones the app asks for (400, 500, 600, 700). Each is one instance of the
variable font, cut with fontTools:

```sh
fonttools varLib.instancer "Figtree[wght].ttf" wght=500 --update-name-table -o Figtree-Medium.ttf
```

and the same with `wght=400`, `600` and `700` for Regular, SemiBold and Bold. With
`--update-name-table`, Medium and SemiBold are named "Figtree Medium" and "Figtree SemiBold" for
old apps and group under the family "Figtree" by their typographic family name.
