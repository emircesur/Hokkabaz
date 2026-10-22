use hk_stt::Word;

use crate::CaptionStyle;

fn stamp(time: f64, decimal: char, precision: u32) -> String {
    let unit = 10_u64.pow(precision);
    let ticks = (time.max(0.0) * unit as f64).round() as u64;
    format!(
        "{:02}:{:02}:{:02}{decimal}{:0width$}",
        ticks / unit / 3600,
        ticks / unit / 60 % 60,
        ticks / unit % 60,
        ticks % unit,
        width = precision as usize
    )
}

fn text(word: &str) -> String {
    word.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace(['\r', '\n'], " ")
}

fn groups(words: &[Word], size: usize) -> Vec<&[Word]> {
    let mut out = Vec::new();
    let mut start = 0;
    for i in 0..words.len() {
        if i + 1 == words.len()
            || i + 1 - start >= size.max(1)
            || words[i + 1].start - words[i].end > 0.6
            || words[i].text.ends_with(['.', '!', '?'])
        {
            out.push(&words[start..=i]);
            start = i + 1;
        }
    }
    out
}

pub fn srt(words: &[Word], style: &CaptionStyle) -> String {
    groups(words, style.words_per_line)
        .iter()
        .enumerate()
        .map(|(i, g)| {
            format!(
                "{}\n{} --> {}\n{}\n\n",
                i + 1,
                stamp(g[0].start, ',', 3),
                stamp(g[g.len() - 1].end, ',', 3),
                g.iter()
                    .map(|w| text(&w.text))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        })
        .collect()
}

pub fn vtt(words: &[Word], style: &CaptionStyle) -> String {
    let mut output = String::from("WEBVTT\n\n");
    for group in groups(words, style.words_per_line) {
        output.push_str(&format!(
            "{} --> {}\n{}\n\n",
            stamp(group[0].start, '.', 3),
            stamp(group[group.len() - 1].end, '.', 3),
            group
                .iter()
                .map(|w| text(&w.text))
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    output
}

fn color(hex: &str) -> String {
    format!("&H00{}{}{}", &hex[5..7], &hex[3..5], &hex[1..3])
}
fn escape(word: &str) -> String {
    word.replace('\\', "＼")
        .replace('{', "｛")
        .replace('}', "｝")
        .replace(['\n', '\r'], " ")
}

pub fn ass(words: &[Word], style: &CaptionStyle, size: (u32, u32)) -> String {
    let mut output = format!(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: {}\nPlayResY: {}\nWrapStyle: 2\n\n[V4+ Styles]\nFormat: Name,Fontname,Fontsize,PrimaryColour,SecondaryColour,OutlineColour,BackColour,Bold,Italic,Underline,StrikeOut,ScaleX,ScaleY,Spacing,Angle,BorderStyle,Outline,Shadow,Alignment,MarginL,MarginR,MarginV,Encoding\nStyle: Default,{},{},{},{},&H00111111,&H80000000,-1,0,0,0,100,100,0,0,1,{},0,2,40,40,{},1\n\n[Events]\nFormat: Layer,Start,End,Style,Name,MarginL,MarginR,MarginV,Effect,Text\n",
        size.0,
        size.1,
        style.font,
        style.font_size,
        color(&style.highlight),
        color(&style.color),
        style.outline,
        style.margin_bottom
    );
    for group in groups(words, style.words_per_line) {
        let start = group[0].start;
        let end = group[group.len() - 1].end;
        let mut line = String::new();
        if style.animated {
            line.push_str("{\\fad(60,80)}");
        }
        let mut previous = start;
        for (i, word) in group.iter().enumerate() {
            if i > 0 {
                line.push(' ');
            }
            let gap = ((word.start - previous).max(0.0) * 100.0).round() as u64;
            if gap > 0 {
                line.push_str(&format!("{{\\k{gap}}} "));
            }
            let ticks = ((word.end - word.start) * 100.0).round().max(1.0) as u64;
            let escaped = escape(&word.text);
            line.push_str(&format!(
                "{{\\kf{ticks}}}{}",
                if style.uppercase {
                    escaped.to_uppercase()
                } else {
                    escaped
                }
            ));
            previous = word.end;
        }
        output.push_str(&format!(
            "Dialogue: 0,{},{},Default,,0,0,0,,{}\n",
            stamp(start, '.', 2),
            stamp(end, '.', 2),
            line
        ));
    }
    output
}
