#!/usr/bin/env python3
"""Generate the benchmark overview chart as light and dark SVGs.

Five stacked panels, each on its own scale: session tokens, billed cost
(segmented into the work and the fixed context), session time, screenshot
tokens, and approval prompts. Data source:
bench/results/side-by-side-20261008-095818/session-test.md and the README
benchmark table. Re-run this script after any data change instead of
hand-editing the SVGs.
"""

import os

TITLE = "Measured side by side"
SUBTITLE = "Same prompts, same model (Claude Sonnet 5), one run each. Lower is better."

SESSION_TOKENS_PANEL = {
    "type": "simple",
    "title": "Tokens for one design session: a 4-step landing page, screenshot after each step",
    "turbofig_value": 133500,
    "console_value": 374800,
    "turbofig_label": "134K",
    "console_label": "375K (2.8× more)",
    "note": None,
}

BILLED_COST_PANEL = {
    "type": "stacked",
    "title": "Billed cost for that session, in input-token units",
    "turbofig_work": 89400,
    "turbofig_tool": 138600,
    "console_work": 163600,
    "console_tool": 186200,
    "turbofig_total_label": "228K",
    "console_total_label": "350K (35% more)",
    "turbofig_work_label": "89K",
    "console_work_label": "164K",
    "note": (
        "Solid = the work. Light = fixed context every call re-reads, "
        "cached at a tenth of the price."
    ),
}

SESSION_TIME_PANEL = {
    "type": "simple",
    "title": "Time for that session",
    "turbofig_value": 2.9,
    "console_value": 5.9,
    "turbofig_label": "2.9 min",
    "console_label": "5.9 min (includes approval waits)",
    "note": None,
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
    "title": "Approval prompts in 4 small jobs",
    "turbofig_value": 0,
    "console_value": 6,
    "turbofig_label": "0",
    "console_label": "6",
    "note": None,
}

PANELS = [SESSION_TOKENS_PANEL, BILLED_COST_PANEL, SESSION_TIME_PANEL, SCREENSHOT_PANEL, APPROVALS_PANEL]

FOOTER = (
    "Session figures leave out time spent recovering from tool failures, on both sides. "
    "Details: session-test.md in the benchmark results."
)

WIDTH = 860
FONT = "-apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif"

# Layout
PLOT_LEFT = 20
RIGHT_GUTTER = 230
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
NOTE_H = 18
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


def render_panel(parts, panel, y, c):
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

    def work_label(row_y, work_w, label):
        # Label the work segment inside it if there is room, just after it
        # if there is a little room, otherwise skip it.
        if work_w >= 30:
            cx = PLOT_LEFT + work_w / 2
            cy = row_y + BAR_H / 2 + 3.5
            parts.append(
                f'<text x="{cx:.1f}" y="{cy:.1f}" font-size="10.5" fill="#ffffff" '
                f'text-anchor="middle">{label}</text>'
            )
        elif work_w >= 20:
            tx = PLOT_LEFT + work_w + 4
            cy = row_y + BAR_H - 2.5
            parts.append(
                f'<text x="{tx:.1f}" y="{cy:.1f}" font-size="10.5" '
                f'fill="{c["text_secondary"]}">{label}</text>'
            )

    def draw_row(row_y, work_w, tool_w, solid, tint, work_lbl, total_lbl):
        parts.append(flat_rect(PLOT_LEFT, row_y, work_w, BAR_H, solid))
        work_label(row_y, work_w, work_lbl)
        tool_x = PLOT_LEFT + work_w + SEGMENT_GAP
        parts.append(bar(tool_x, row_y, tool_w, BAR_H, tint))
        end_x = tool_x + tool_w
        parts.append(
            f'<text x="{end_x + 8:.1f}" y="{row_y + BAR_H - 2.5:.1f}" font-size="11.5" '
            f'fill="{c["text_secondary"]}">{total_lbl}</text>'
        )

    draw_row(
        tf_y, tf_work_w, tf_tool_w, c["turbofig"], c["turbofig_tint"],
        panel["turbofig_work_label"], panel["turbofig_total_label"],
    )
    draw_row(
        cs_y, cs_work_w, cs_tool_w, c["console"], c["console_tint"],
        panel["console_work_label"], panel["console_total_label"],
    )

    y = bars_top + BARS_H

    if panel["note"]:
        swatch_y = y + 5
        sx = PLOT_LEFT
        parts.append(f'<rect x="{sx}" y="{swatch_y}" width="9" height="9" rx="1.5" fill="{c["console"]}" />')
        parts.append(
            f'<text x="{sx + 14}" y="{y + 13:.1f}" font-size="11" fill="{c["text_secondary"]}">'
            "Solid = the work.</text>"
        )
        tx = sx + 150
        parts.append(f'<rect x="{tx}" y="{swatch_y}" width="9" height="9" rx="1.5" fill="{c["console_tint"]}" />')
        parts.append(
            f'<text x="{tx + 14}" y="{y + 13:.1f}" font-size="11" fill="{c["text_secondary"]}">'
            "Light = fixed context every call re-reads, cached at a tenth of the price.</text>"
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
    parts.append("<title>turbofig vs figma-console-mcp, measured side by side</title>")
    parts.append(
        "<desc>Five panels comparing turbofig to figma-console-mcp, one run "
        "each. Tokens for one design session, a 4-step landing page with a "
        "screenshot after each step: turbofig 134,000 tokens, "
        "figma-console-mcp 375,000 tokens (2.8 times more). Billed cost for "
        "that session, in input-token units: turbofig 228,000 total (89,400 "
        "for the work, 138,600 for the fixed context), figma-console-mcp "
        "350,000 total, 35 percent more (163,600 for the work, 186,200 for "
        "the fixed context). Time for that session: turbofig 2.9 minutes, "
        "figma-console-mcp 5.9 minutes (includes approval waits). Tokens "
        "for one screenshot of the same frame, default settings: turbofig "
        "1,225 tokens at 1200 pixels, figma-console-mcp 3,264 tokens at "
        "2000 pixels. Approval prompts in 4 small jobs: turbofig 0, "
        "figma-console-mcp 6.</desc>"
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
            y = render_panel(parts, panel, y, c)
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
