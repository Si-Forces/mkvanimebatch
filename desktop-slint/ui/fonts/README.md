# Fonts

The variable TrueType fonts the Slint host embeds in its binary (`import` in `app.slint`, Q108):
the same three families as the webview's `desktop-tauri/ui/fonts`, whose `.woff2` files are latin
subsets that Slint cannot load. These are the full upstream fonts, every script included, each
with one `wght` axis. All three are under the SIL Open Font License 1.1; the `OFL-*.txt` texts
are copied from `desktop-tauri/ui/fonts` and ship in every package.

Fetched on 2026-10-23 from [google/fonts](https://github.com/google/fonts) at commit
`e7db6ca603fd4862010383ff17ffd6cc4ffac1c0`, renamed without the brackets:

| File | Source in google/fonts | Upstream (google/fonts `METADATA.pb`) | `wght` | SHA-256 |
|---|---|---|---|---|
| `Onest-wght.ttf` | `ofl/onest/Onest[wght].ttf` | [simpals/onest](https://github.com/simpals/onest) `8739b1910618a15335e4cc48842052d0ee739ade` (2.001) | 100–900 | `966c5c29b4755da84b6854d5c21dd4eaa2420225d0e9874de602de176d4a9f31` |
| `Unbounded-wght.ttf` | `ofl/unbounded/Unbounded[wght].ttf` | [googlefonts/unbounded](https://github.com/googlefonts/unbounded) `f3ec43228a864a72487e41552e2140efab9884ea` | 200–900 | `323b511be380c8d474ef030686b71aedde501f8d9cd46da558b7c40454372c3f` |
| `JetBrainsMono-wght.ttf` | `ofl/jetbrainsmono/JetBrainsMono[wght].ttf` | [JetBrains/JetBrainsMono](https://github.com/JetBrains/JetBrainsMono) `19371302b95d218af43299bce79ddbddd0bc364d` | 100–800 | `48715a42ec242c21e9f02692891e147d022299a52e48d5e413e1a942193ffeda` |

`https://raw.githubusercontent.com/google/fonts/<commit>/<source>` downloads each one (brackets
URL-encoded as `%5B`/`%5D`). The host's `fonts_resolve_their_weights` test checks that the weights
resolve from the variable axis through the software renderer only; the Skia renderer the app
ships with is checked by hand (weights 200, 400 and 800 on the Skia OpenGL build). If Skia ignores
the axis, switch to static instances per weight (#133).
