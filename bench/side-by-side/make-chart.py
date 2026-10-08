#!/usr/bin/env python3
"""Generate the benchmark overview chart as light and dark SVGs.

Four stacked panels, each on its own scale: tokens (segmented into the
work and the tool list), screenshot tokens, approval prompts, and startup
latency. Data source: bench/results/side-by-side-20261008-095818/ and the
README benchmark table. Re-run this script after any data change instead
of hand-editing the SVGs.
"""

import os

TITLE = "Where the difference comes from"
SUBTITLE = "One Mac, macOS 15, October 2026. Lower is better."

TOKENS_PANEL = {
    "type": "stacked",
    "title": "Tokens across the same 4 jobs, if all tools are loaded (estimate)",
    "turbofig_work": 50400,
    "turbofig_tool": 6800,
    "console_work": 38500,
    "console_tool": 366000,
    "note": (
        "Solid = the work (measured). "
        "Light = tool list re-read on each of 10 AI calls (estimate)."
    ),
}

SCREENSHOT_PANEL = {
    "type": "simple",
    "title": "Tokens for one screenshot of the same frame, default settings",
    "turbofig_value": 1225,
    "console_value": 3264,
    "turbofig_label": "1,225 (1200 px)",
    "console_label": "3,264 (2000 px)",
    "note": None,
}

APPROVALS_PANEL = {
    "type": "simple",
    "title": "Approval prompts in the same 4 jobs",
    "turbofig_value": 0,
    "console_value": 6,
    "turbofig_label": "0",
    "console_label": "6",
    "note": None,
}

STARTUP_PANEL = {
    "type": "simple",
    "title": "Ready when your AI starts",
    "turbofig_value": 0.07,
    "console_value": 1.9,
    "turbofig_label": "0.07 s",
    "console_label": "1.9 s",
    "note": "figma-console-mcp: 29 s on a first run",
}

PANELS = [TOKENS_PANEL, SCREENSHOT_PANEL, APPROVALS_PANEL, STARTUP_PANEL]

FOOTER = (
    "Repeated tool lists are cached, so the cost gap is smaller than the "
    "token gap. See recount.md in the benchmark results."
)

WIDTH = 860
FONT = "-apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif"

# Layout
PLOT_LEFT = 20
RIGHT_GUTTER = 190
PLOT_WIDTH = WIDTH - PLOT_LEFT - RIGHT_GUTTER
SEGMENT_GAP = 2  # surface-colour gap between the two segments of a stacked bar

BAR_H = 13
BAR_GAP = 2  # between the two bars in a panel

TITLE_Y = 30
SUBTITLE_Y = 50
LEGEND_Y = 72
PANELS_TOP = 96

PANEL_TITLE_H = 20
BARS_H = BAR_H * 2 + BAR_GAP
NOTE_H = 20
PANEL_GAP = 22
FOOTER_GAP = 26
FOOTER_H = 20

COLORS = {
    "light": {
        "turbofig": "#2a78d6",
        "turbofig_tint": "#9fc2ed",
        "console": "#eb6834",
        "console_tint": "#f6bba4",
        "surface": "#fcfcfb",
        "text_primary": "#0b0b0b",
        "text_secondary": "#52514e",
        "grid": "#e6e5e1",
    },
    "dark": {
        "turbofig": "#3987e5",
        "turbofig_tint": "#a6c9f3",
        "console": "#d95926",
        "console_tint": "#eeb49d",
        "surface": "#1a1a19",
        "text_primary": "#ffffff",
        "text_secondary": "#c3c2b7",
        "grid": "#333331",
    },
}


def fmt_k(value):
    return f"~{int(value / 1000 + 0.5)}K"


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


def flat_rect(x, y, w, h, color):
    """Square-cornered rectangle, for a segment that is not the bar's data end."""
    return f'<rect x="{x:.1f}" y="{y:.1f}" width="{max(w, 0):.1f}" height="{h}" fill="{color}" />'


def panel_height(panel):
    h = PANEL_TITLE_H + BARS_H
    if panel["note"]:
        h += NOTE_H
    return h


def render_simple_panel(parts, panel, y, c):
    bars_top = y + PANEL_TITLE_H
    tf_y = bars_top
    cs_y = bars_top + BAR_H + BAR_GAP

    axis_max = max(panel["turbofig_value"], panel["console_value"]) or 1
    tf_w = (panel["turbofig_value"] / axis_max) * PLOT_WIDTH
    cs_w = (panel["console_value"] / axis_max) * PLOT_WIDTH

    parts.append(bar(PLOT_LEFT, tf_y, tf_w, BAR_H, c["turbofig"]))
    parts.append(bar(PLOT_LEFT, cs_y, cs_w, BAR_H, c["console"]))

    parts.append(
        f'<text x="{PLOT_LEFT + tf_w + 8:.1f}" y="{tf_y + BAR_H - 2.5:.1f}" font-size="11.5" '
        f'fill="{c["text_secondary"]}">{panel["turbofig_label"]}</text>'
    )
    parts.append(
        f'<text x="{PLOT_LEFT + cs_w + 8:.1f}" y="{cs_y + BAR_H - 2.5:.1f}" font-size="11.5" '
        f'fill="{c["text_secondary"]}">{panel["console_label"]}</text>'
    )

    y = bars_top + BARS_H

    if panel["note"]:
        parts.append(
            f'<text x="{PLOT_LEFT}" y="{y + 13:.1f}" font-size="11" '
            f'fill="{c["text_secondary"]}">{panel["note"]}</text>'
        )
        y += NOTE_H

    return y


def render_stacked_panel(parts, panel, y, c):
    bars_top = y + PANEL_TITLE_H
    tf_y = bars_top
    cs_y = bars_top + BAR_H + BAR_GAP

    tf_total = panel["turbofig_work"] + panel["turbofig_tool"]
    cs_total = panel["console_work"] + panel["console_tool"]
    axis_max = max(tf_total, cs_total) or 1
    data_width = PLOT_WIDTH - SEGMENT_GAP

    def segment_widths(work, tool):
        return (work / axis_max) * data_width, (tool / axis_max) * data_width

    tf_work_w, tf_tool_w = segment_widths(panel["turbofig_work"], panel["turbofig_tool"])
    cs_work_w, cs_tool_w = segment_widths(panel["console_work"], panel["console_tool"])

    def draw_row(row_y, work_w, tool_w, solid, tint, total_value):
        parts.append(flat_rect(PLOT_LEFT, row_y, work_w, BAR_H, solid))
        tool_x = PLOT_LEFT + work_w + SEGMENT_GAP
        parts.append(bar(tool_x, row_y, tool_w, BAR_H, tint))
        end_x = tool_x + tool_w
        parts.append(
            f'<text x="{end_x + 8:.1f}" y="{row_y + BAR_H - 2.5:.1f}" font-size="11.5" '
            f'fill="{c["text_secondary"]}">{fmt_k(total_value)}</text>'
        )

    draw_row(tf_y, tf_work_w, tf_tool_w, c["turbofig"], c["turbofig_tint"], tf_total)
    draw_row(cs_y, cs_work_w, cs_tool_w, c["console"], c["console_tint"], cs_total)

    y = bars_top + BARS_H

    if panel["note"]:
        swatch_y = y + 5
        sx = PLOT_LEFT
        parts.append(f'<rect x="{sx}" y="{swatch_y}" width="9" height="9" rx="1.5" fill="{c["console"]}" />')
        parts.append(
            f'<text x="{sx + 14}" y="{y + 13:.1f}" font-size="11" fill="{c["text_secondary"]}">'
            "Solid = the work (measured).</text>"
        )
        tx = sx + 190
        parts.append(f'<rect x="{tx}" y="{swatch_y}" width="9" height="9" rx="1.5" fill="{c["console_tint"]}" />')
        parts.append(
            f'<text x="{tx + 14}" y="{y + 13:.1f}" font-size="11" fill="{c["text_secondary"]}">'
            "Light = tool list re-read on each of 10 AI calls (estimate).</text>"
        )
        y += NOTE_H

    return y


def render(theme_name):
    c = COLORS[theme_name]
    parts = []

    # First pass: compute total height.
    y = PANELS_TOP
    for i, panel in enumerate(PANELS):
        y += panel_height(panel)
        if i < len(PANELS) - 1:
            y += PANEL_GAP
    total_height = y + FOOTER_GAP + FOOTER_H

    parts.append(
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {WIDTH} {total_height}" '
        f'width="{WIDTH}" height="{total_height}" font-family="{FONT}">'
    )
    parts.append("<title>turbofig vs figma-console-mcp, where the difference comes from</title>")
    parts.append(
        "<desc>Four panels comparing turbofig to figma-console-mcp. "
        "Tokens across the same 4 jobs, if all tools are loaded (estimate): "
        "turbofig about 57,200 tokens total (50,400 for the work, about "
        "6,800 for the tool list), figma-console-mcp about 404,500 tokens "
        "total (38,500 for the work, about 366,000 for the tool list). "
        "Tokens for one screenshot of the same frame, default settings: "
        "turbofig 1,225 tokens at 1200 pixels, figma-console-mcp 3,264 "
        "tokens at 2000 pixels. Approval prompts across the same 4 jobs: "
        "turbofig 0, figma-console-mcp 6. Ready when your AI starts: "
        "turbofig 0.07 seconds, figma-console-mcp 1.9 seconds (29 seconds "
        "on a first run).</desc>"
    )
    parts.append(f'<rect x="0" y="0" width="{WIDTH}" height="{total_height}" fill="{c["surface"]}" />')

    # Title + subtitle
    parts.append(
        f'<text x="20" y="{TITLE_Y}" font-size="17" font-weight="700" fill="{c["text_primary"]}">{TITLE}</text>'
    )
    parts.append(
        f'<text x="20" y="{SUBTITLE_Y}" font-size="12.5" fill="{c["text_secondary"]}">{SUBTITLE}</text>'
    )

    # Legend
    parts.append(f'<rect x="20" y="{LEGEND_Y - 10}" width="10" height="10" rx="2" fill="{c["turbofig"]}" />')
    parts.append(f'<text x="36" y="{LEGEND_Y - 1}" font-size="12.5" fill="{c["text_primary"]}">turbofig</text>')
    parts.append(f'<rect x="120" y="{LEGEND_Y - 10}" width="10" height="10" rx="2" fill="{c["console"]}" />')
    parts.append(f'<text x="136" y="{LEGEND_Y - 1}" font-size="12.5" fill="{c["text_primary"]}">figma-console-mcp</text>')

    y = PANELS_TOP
    for panel in PANELS:
        title_y = y + 14
        parts.append(
            f'<text x="20" y="{title_y:.1f}" font-size="13" font-weight="600" '
            f'fill="{c["text_primary"]}">{panel["title"]}</text>'
        )

        if panel["type"] == "stacked":
            y = render_stacked_panel(parts, panel, y, c)
        else:
            y = render_simple_panel(parts, panel, y, c)

        y += PANEL_GAP

    footer_y = y - PANEL_GAP + FOOTER_GAP
    parts.append(
        f'<text x="20" y="{footer_y:.1f}" font-size="11" fill="{c["text_secondary"]}">{FOOTER}</text>'
    )

    parts.append("</svg>")
    return "\n".join(parts)


def main():
    out_dir = os.path.join(os.path.dirname(__file__), "..", "..", "docs")
    out_dir = os.path.normpath(out_dir)
    os.makedirs(out_dir, exist_ok=True)

    for theme in ("light", "dark"):
        path = os.path.join(out_dir, f"benchmark-overview-{theme}.svg")
        svg = render(theme)
        with open(path, "w") as f:
            f.write(svg + "\n")
        print(f"wrote {path}")


if __name__ == "__main__":
    main()
