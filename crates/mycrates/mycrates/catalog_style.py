CSS = r"""
:root { color-scheme: light dark; --bg: #fff; --fg: #171717; --mute: #6b6b6b; --line: #ebebeb; --void: #b42318; }
@media (prefers-color-scheme: dark) { :root { --bg: #0d0d0d; --fg: #ededed; --mute: #9b9b9b; --line: #242424; --void: #f97066; } }
* { box-sizing: border-box; }
html { background: var(--bg); color: var(--fg); }
body { margin: 0 auto; max-width: 46rem; padding: 0 1.5rem 4rem; font: 400 0.9375rem/1.6 ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif; -webkit-font-smoothing: antialiased; overflow-wrap: anywhere; }
a { color: inherit; text-decoration: underline; text-decoration-color: var(--line); text-underline-offset: 0.2em; text-decoration-thickness: 1px; }
a:hover { text-decoration-color: currentColor; }
:focus-visible { outline: 2px solid var(--fg); outline-offset: 3px; border-radius: 2px; }
code, .mono { font-family: ui-monospace, "SF Mono", Menlo, Consolas, monospace; font-size: 0.875em; }
.mute { color: var(--mute); }
header { padding: 2rem 0 4rem; font-weight: 600; letter-spacing: -0.01em; }
h1 { font-size: 1.5rem; line-height: 1.3; font-weight: 600; letter-spacing: -0.02em; margin: 0 0 0.5rem; max-width: 30rem; }
h2 { font-size: 0.9375rem; font-weight: 600; margin: 2.5rem 0 0.5rem; }
.lede { margin: 0 0 3rem; color: var(--mute); }
.notice { margin: -1.5rem 0 1.5rem; color: var(--void); }
input[type="search"]:not([hidden]) { display: block; width: 100%; margin: 0 0 1.5rem; padding: 0.6rem 0.8rem; font: inherit; color: inherit; background: transparent; border: 1px solid var(--line); border-radius: 6px; }
input[type="search"]::placeholder { color: var(--mute); }
.registries { display: grid; grid-template-columns: repeat(auto-fit, minmax(11rem, 1fr)); gap: 1.5rem; margin: 0 0 3.5rem; padding: 0; list-style: none; }
.registries b { display: block; font-weight: 600; }
.registries span { color: var(--mute); }
table { width: 100%; border-collapse: collapse; font-variant-numeric: tabular-nums; }
th { text-align: left; font-weight: 400; color: var(--mute); padding: 0 1rem 0.6rem 0; border-bottom: 1px solid var(--line); }
td { padding: 0.65rem 1rem 0.65rem 0; border-bottom: 1px solid var(--line); vertical-align: baseline; }
th:last-child, td:last-child { padding-right: 0; }
.r { text-align: right; }
.pkg { font-family: ui-monospace, "SF Mono", Menlo, Consolas, monospace; font-size: 0.875rem; }
.vers { color: var(--mute); font-family: ui-monospace, "SF Mono", Menlo, Consolas, monospace; font-size: 0.8125rem; line-height: 1.7; }
.vers b { color: var(--fg); }
.yanked { color: var(--void); text-decoration: line-through; }
.detail { display: none; }
.detail:target { display: block; }
main:has(.detail:target) .index { display: none; }
.back { margin: 0 0 2rem; }
.refs { margin: 0; padding: 0; list-style: none; }
.refs li { padding: 0.35rem 0; border-bottom: 1px solid var(--line); }
.refs .mute { margin-left: 0.75rem; }\ndetails { margin: 1rem 0 0; }\nsummary { cursor: pointer; font-weight: 600; }\ndetails[open] summary { margin-bottom: 0.5rem; }
footer { margin-top: 3rem; display: flex; flex-wrap: wrap; gap: 0.25rem 1.5rem; color: var(--mute); font-size: 0.875rem; }
@media (max-width: 640px) { header { padding-bottom: 2.5rem; } .hide { display: none; } }
"""
