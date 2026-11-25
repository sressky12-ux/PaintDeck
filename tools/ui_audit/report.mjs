export function defectKey(scenario, theme, width, defect) {
  return [scenario, theme, width, defect.kind, defect.selector].join('|');
}

export function newDefects(prevReport, curReport) {
  const previous = new Set();
  for (const { scenario, theme, width, defects } of prevReport.shots) {
    for (const d of defects) previous.add(defectKey(scenario, theme, width, d));
  }
  const added = [];
  for (const { scenario, theme, width, defects } of curReport.shots) {
    for (const d of defects) {
      if (!previous.has(defectKey(scenario, theme, width, d))) added.push({ scenario, theme, width, ...d });
    }
  }
  return added;
}

function cell(value) {
  return String(value).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
    .replace(/\|/g, '\\|').replace(/\s+/g, ' ');
}

export function renderMarkdown({ meta, shots }) {
  const lines = ['# PaintDeck 介面檢查', '', [meta.date, meta.base, meta.style || '預設樣式', meta.lang].join(' · ')];
  for (const { scenario, theme, width, file, defects, classes, viewerRatio, actsRows } of shots) {
    lines.push('', '## ' + [scenario, theme, width].join(' · '), '', '[截圖](' + encodeURIComponent(file) + ')', '');
    if (viewerRatio != null) lines.push(`viewerRatio ${viewerRatio.toFixed(3)} · actsRows ${actsRows}`, '');
    if (scenario === 'director:edit') lines.push('剪輯台僅截圖。');
    else if (!defects.length) lines.push('未偵測到缺陷。');
    else {
      lines.push('| 種類 | 選擇器 | 文字 | 位置與尺寸 (x, y, w, h) |', '| --- | --- | --- | --- |');
      for (const d of defects) lines.push('| ' + [d.kind, d.selector, d.text, d.rect.map(Math.round).join(', ')].map(cell).join(' | ') + ' |');
    }
    const counts = Object.entries(classes).sort(([a, n], [b, m]) => m - n || a.localeCompare(b));
    if (counts.length) lines.push('', '可見元件 class 次數', '', ...counts.map(([name, n]) => '- ' + cell(name) + ' × ' + n));
  }
  return lines.join('\n') + '\n';
}
