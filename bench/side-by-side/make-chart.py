#!/usr/bin/env python3
"""Generate the benchmark bar chart as light and dark SVGs.

Data source: bench/results/side-by-side-20261008-095818/interactive/
rerun-results.md and results.md (Run B NET tokens, checked against the
README benchmark table). Re-run this script after any data change instead
of hand-editing the SVGs.
"""

import os

JOBS = [
    {"name": "Red 200×200 square", "turbofig": 13711, "console": 52749, "ratio": "3.9× fewer"},
    {"name": "Lay out 40 slides", "turbofig": 13025, "console": 39200, "ratio": "3.0× fewer"},
    {"name": "Recolour text on 40 slides", "turbofig": 18804, "console": 34692, "ratio": "1.9× fewer"},
    {"name": "Hero section", "turbofig": 27150, "console": 41876, "ratio": "1.5× fewer"},
]

TITLE = "Tokens per job (lower is better)"
SUBTITLE = (
    "Same prompts, same model (Claude Sonnet 5). turbofig used 2.3× "
    "fewer tokens across all 4 jobs."
)

WIDTH = 860
HEIGHT = 300
FONT = "-apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif"

# Layout
LEFT_LABEL_W = 190
RIGHT_RATIO_W = 110
PLOT_LEFT = LEFT_LABEL_W
PLOT_RIGHT = WIDTH - RIGHT_RATIO_W - 20
PLOT_WIDTH = PLOT_RIGHT - PLOT_LEFT

BAR_H = 14
BAR_GAP = 2  # between the two bars in a row
ROW_GAP = 8  # extra gap between rows
ROW_H = BAR_H * 2 + BAR_GAP + ROW_GAP

PLOT_TOP = 110
AXIS_MAX = 60000
TICKS = [0, 20000, 40000, 60000]

COLORS = {
    "light": {
        "turbofig": "#2a78d6",
        "console": "#eb6834",
        "surface": "#fcfcfb",
        "text_primary": "#0b0b0b",
        "text_secondary": "#52514e",
        "grid": "#e6e5e1",
    },
    "dark": {
        "turbofig": "#3987e5",
        "console": "#d95926",
        "surface": "#1a1a19",
        "text_primary": "#ffffff",
        "text_secondary": "#c3c2b7",
        "grid": "#333331",
    },
}


def fmt_k(value):
    k = value / 1000.0
    if k == int(k):
        return f"{int(k)}K"
    return f"{int(value / 100 + 0.5) / 10:.1f}K"  # round half up, like the README table


def x_for(value):
    return PLOT_LEFT + (value / AXIS_MAX) * PLOT_WIDTH


def bar(x, y, w, h, color, radius=4):
    """Rounded rectangle, rounded only on the right (data) end."""
    if w <= radius:
        return f'<rect x="{x:.1f}" y="{y:.1f}" width="{w:.1f}" height="{h}" fill="{color}" />'
    return (
        f'<path d="M {x:.1f} {y:.1f} '
        f'H {x + w - radius:.1f} '
        f'A {radius} {radius} 0 0 1 {x + w:.1f} {y + radius:.1f} '
        f'V {y + h - radius:.1f} '
        f'A {radius} {radius} 0 0 1 {x + w - radius:.1f} {y + h:.1f} '
        f'H {x:.1f} Z" fill="{color}" />'
    )


def render(theme_name):
    c = COLORS[theme_name]
    parts = []
    parts.append(
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {WIDTH} {HEIGHT}" '
        f'width="{WIDTH}" height="{HEIGHT}" font-family="{FONT}">'
    )
    parts.append(
        "<title>Tokens per job, turbofig vs figma-console-mcp</title>"
    )
    parts.append(
        "<desc>Grouped horizontal bar chart comparing net tokens per job "
        "between turbofig and figma-console-mcp across 4 Figma jobs: "
        "red square, 40-slide layout, recolour, and hero section. "
        "turbofig used fewer tokens in every job.</desc>"
    )
    parts.append(f'<rect x="0" y="0" width="{WIDTH}" height="{HEIGHT}" fill="{c["surface"]}" />')

    # Title + subtitle
    parts.append(
        f'<text x="20" y="30" font-size="17" font-weight="700" fill="{c["text_primary"]}">{TITLE}</text>'
    )
    parts.append(
        f'<text x="20" y="50" font-size="12.5" fill="{c["text_secondary"]}">{SUBTITLE}</text>'
    )

    # Legend
    legend_y = 72
    parts.append(f'<rect x="20" y="{legend_y - 10}" width="10" height="10" rx="2" fill="{c["turbofig"]}" />')
    parts.append(f'<text x="36" y="{legend_y - 1}" font-size="12.5" fill="{c["text_primary"]}">turbofig</text>')
    parts.append(f'<rect x="120" y="{legend_y - 10}" width="10" height="10" rx="2" fill="{c["console"]}" />')
    parts.append(f'<text x="136" y="{legend_y - 1}" font-size="12.5" fill="{c["text_primary"]}">figma-console-mcp</text>')

    plot_bottom = PLOT_TOP + len(JOBS) * ROW_H - ROW_GAP

    # Gridlines + tick labels
    for tick in TICKS:
        gx = x_for(tick)
        parts.append(
            f'<line x1="{gx:.1f}" y1="{PLOT_TOP - 6}" x2="{gx:.1f}" y2="{plot_bottom + 4}" '
            f'stroke="{c["grid"]}" stroke-width="1" />'
        )
        parts.append(
            f'<text x="{gx:.1f}" y="{plot_bottom + 20}" font-size="11" fill="{c["text_secondary"]}" '
            f'text-anchor="middle">{fmt_k(tick) if tick else "0"}</text>'
        )

    for i, job in enumerate(JOBS):
        row_top = PLOT_TOP + i * ROW_H
        tf_y = row_top
        cs_y = row_top + BAR_H + BAR_GAP
        row_mid = row_top + BAR_H + BAR_GAP / 2

        # Job name (left)
        parts.append(
            f'<text x="{LEFT_LABEL_W - 14}" y="{row_mid + 4:.1f}" font-size="12.5" '
            f'fill="{c["text_primary"]}" text-anchor="end">{job["name"]}</text>'
        )

        tf_w = (job["turbofig"] / AXIS_MAX) * PLOT_WIDTH
        cs_w = (job["console"] / AXIS_MAX) * PLOT_WIDTH

        parts.append(bar(PLOT_LEFT, tf_y, tf_w, BAR_H, c["turbofig"]))
        parts.append(bar(PLOT_LEFT, cs_y, cs_w, BAR_H, c["console"]))

        # Value labels at bar ends
        parts.append(
            f'<text x="{PLOT_LEFT + tf_w + 8:.1f}" y="{tf_y + BAR_H - 3:.1f}" font-size="11" '
            f'fill="{c["text_secondary"]}">{fmt_k(job["turbofig"])}</text>'
        )
        parts.append(
            f'<text x="{PLOT_LEFT + cs_w + 8:.1f}" y="{cs_y + BAR_H - 3:.1f}" font-size="11" '
            f'fill="{c["text_secondary"]}">{fmt_k(job["console"])}</text>'
        )

        # Ratio, far right of the row
        parts.append(
            f'<text x="{WIDTH - 20}" y="{row_mid + 4:.1f}" font-size="12.5" font-weight="700" '
            f'fill="{c["text_primary"]}" text-anchor="end">{job["ratio"]}</text>'
        )

    parts.append("</svg>")
    return "\n".join(parts)


def main():
    out_dir = os.path.join(os.path.dirname(__file__), "..", "..", "docs")
    out_dir = os.path.normpath(out_dir)
    os.makedirs(out_dir, exist_ok=True)

    for theme in ("light", "dark"):
        svg = render(theme)
        path = os.path.join(out_dir, f"benchmark-tokens-{theme}.svg")
        with open(path, "w") as f:
            f.write(svg + "\n")
        print(f"wrote {path}")


if __name__ == "__main__":
    main()
