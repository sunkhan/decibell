(() => {
  const lines = [];
  const walk = (el, depth, parentKey) => {
    const r = el.getBoundingClientRect(); const cs = getComputedStyle(el);
    if (cs.display === 'none' || cs.visibility === 'hidden') return;
    const onScreen = r.bottom > 96 && r.top < innerHeight && r.right > 328 && r.width >= 8 && r.height >= 8;
    const key = [Math.round(r.x), Math.round(r.y), Math.round(r.width), Math.round(r.height)].join(',');
    let d = depth;
    if (onScreen && key !== parentKey && r.left >= 328) {
      const cls = (el.getAttribute('class') || '').split(/\s+/).filter(Boolean).slice(0, 7).join('.');
      const bits = [];
      if (cs.backgroundColor !== 'rgba(0, 0, 0, 0)') bits.push('bg ' + cs.backgroundColor);
      if (cs.borderRadius !== '0px') bits.push('r ' + cs.borderRadius);
      if (cs.borderTopWidth !== '0px' || cs.borderLeftWidth !== '0px') bits.push('bd ' + cs.borderTopWidth + '/' + cs.borderLeftWidth + ' ' + cs.borderTopColor);
      if (cs.paddingTop !== '0px' || cs.paddingLeft !== '0px') bits.push('pad ' + cs.paddingTop + ' ' + cs.paddingRight + ' ' + cs.paddingBottom + ' ' + cs.paddingLeft);
      const txt = [...el.childNodes].filter((n) => n.nodeType === 3 && n.textContent.trim()).map((n) => n.textContent.trim()).join(' ').slice(0, 28);
      if (txt || el.tagName === 'INPUT' || el.tagName === 'TEXTAREA') bits.push(`"${txt || el.placeholder || ''}" ${cs.fontFamily.split(',')[0]} ${cs.fontSize}/${cs.lineHeight} w${cs.fontWeight} ${cs.color} ls ${cs.letterSpacing}`);
      if (el.tagName === 'svg') bits.push('svg ' + (el.getAttribute('stroke-width') || '') + ' ' + cs.color);
      lines.push('  '.repeat(depth) + `${el.tagName.toLowerCase()}${cls ? '.' + cls : ''} [${key}] ${bits.join(' | ')}`);
      d = depth + 1;
    }
    if (lines.length < 420) for (const c of el.children) walk(c, d, onScreen ? key : parentKey);
  };
  walk(document.body, 0, '');
  return lines.join('\n');
})()
