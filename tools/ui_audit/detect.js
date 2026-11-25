(() => {
  const px = value => parseFloat(value) || 0;
  const visible = el => el.checkVisibility({ opacityProperty: true, visibilityProperty: true });

  function selector(el) {
    const parts = [];
    while (el && parts.length < 3) {
      const tag = el.localName;
      if (el.id) {
        parts.unshift(tag + '#' + CSS.escape(el.id));
        break;
      }
      parts.unshift([tag, ...Array.from(el.classList).slice(0, 2).map(c => CSS.escape(c))].join('.'));
      el = el.parentElement;
    }
    return parts.join(' > ');
  }

  function painted(style) {
    const color = style.backgroundColor;
    return color !== 'transparent' && !/^rgba\([^)]*,\s*0(?:\.0+)?\)$/.test(color) &&
      !/\/\s*0(?:\.0+)?%?\s*\)$/.test(color);
  }

  function pseudoRect(el, pseudo, box) {
    const s = getComputedStyle(el, pseudo);
    if (!['absolute', 'fixed'].includes(s.position) || ['none', 'normal'].includes(s.content) ||
        s.display === 'none' || s.visibility !== 'visible' || Number(s.opacity) === 0 ||
        (!painted(s) && s.boxShadow === 'none')) return;
    const width = px(s.width), height = px(s.height);
    if (!width || !height) return;
    const fixed = s.position === 'fixed';
    const x = fixed ? 0 : box.left + el.clientLeft, y = fixed ? 0 : box.top + el.clientTop;
    const w = fixed ? innerWidth : el.clientWidth, h = fixed ? innerHeight : el.clientHeight;
    const offset = (value, size) => value.endsWith('%') ? px(value) * size / 100 : px(value);
    const left = s.left === 'auto' && s.right !== 'auto' ? w - offset(s.right, w) - width - px(s.marginRight)
      : offset(s.left, w) + px(s.marginLeft);
    const top = s.top === 'auto' && s.bottom !== 'auto' ? h - offset(s.bottom, h) - height - px(s.marginBottom)
      : offset(s.top, h) + px(s.marginTop);
    return new DOMRect(x + left, y + top, width, height);
  }

  function overlapsText(el, box) {
    const nodes = Array.from(el.childNodes).filter(n => n.nodeType === Node.TEXT_NODE && n.data.trim());
    if (!nodes.length) return false;
    const covers = ['::before', '::after'].map(p => pseudoRect(el, p, box)).filter(Boolean);
    for (const child of el.querySelectorAll('*')) {
      const style = getComputedStyle(child);
      if (style.position === 'absolute' && painted(style) && visible(child)) covers.push(child.getBoundingClientRect());
    }
    if (!covers.length) return false;
    const range = document.createRange();
    return nodes.some(node => {
      range.selectNodeContents(node);
      return Array.from(range.getClientRects()).some(r => covers.some(c =>
        Math.min(r.right, c.right) - Math.max(r.left, c.left) > 1 &&
        Math.min(r.bottom, c.bottom) - Math.max(r.top, c.top) > 1));
    });
  }

  globalThis.__uiAuditDetect = (root = document) => {
    const defects = [];
    const canvas = document.createElement('canvas').getContext('2d');
    const elements = root.querySelectorAll('*');
    for (const el of root.nodeType === Node.ELEMENT_NODE ? [root, ...elements] : elements) {
      const box = el.getBoundingClientRect();
      if (!box.width || !box.height || !visible(el)) continue;
      const s = getComputedStyle(el);
      const option = el instanceof HTMLSelectElement && el.selectedOptions[0];
      const label = (option ? option.text : el.textContent || '').trim();
      function add(kind) {
        defects.push({ kind, selector: selector(el), text: [...label].slice(0, 20).join(''),
          rect: [box.x, box.y, box.width, box.height] });
      }
      if (overlapsText(el, box)) add('overlap');
      let clipped = !option && label && s.overflowX !== 'visible' && s.textOverflow !== 'ellipsis' && el.scrollWidth > el.clientWidth + 1;
      if (option) {
        canvas.font = s.font || [s.fontStyle, s.fontWeight, s.fontSize, s.fontFamily].join(' ');
        // 原生下拉箭頭約 20px；appearance: none 的選單（繪匣的讀數框）沒有箭頭，自訂箭頭已經算在 padding 裡
        const arrow = s.appearance === 'none' ? 0 : 20;
        clipped = canvas.measureText(option.text).width > el.clientWidth - px(s.paddingLeft) - px(s.paddingRight) - arrow + 1;
      }
      if (clipped) add('clip');
      if (box.right > document.documentElement.clientWidth + 1 || box.left < -1) add('overflow');
    }
    return defects;
  };
})();
