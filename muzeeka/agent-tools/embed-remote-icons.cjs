const fs = require('fs');
const path = require('path');

const dir = path.join(__dirname, '..', 'static', 'icons');
const names = [
  'play',
  'pause',
  'playbackward',
  'playforward',
  'shuffle',
  'noshuffle',
  'repeat',
  'norepeat',
  'repeatplaylist',
  'mute',
  'volmin',
  'volmed',
  'volmax',
];

const out = {};
for (const n of names) {
  let s = fs.readFileSync(path.join(dir, `${n}.svg`), 'utf8').trim();
  s = s.replace(/fill="currentColor"/g, 'fill="#FEFEFE"');
  s = s.replace(/\s+/g, ' ');
  out[n] = s;
}

const body = Object.entries(out)
  .map(([k, svg]) => `      ${JSON.stringify(k)}: ${JSON.stringify(svg)}`)
  .join(',\n');

const js = `    // SVG icons inlined from static/icons (no network / no disk paths on phone)
    const ICONS_SVG = {
${body}
    };

    function iconSrc(name) {
      const svg = ICONS_SVG[name];
      if (!svg) return '';
      return 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(svg);
    }
`;

const outPath = path.join(__dirname, 'remote-icons-snippet.js');
fs.writeFileSync(outPath, js, 'utf8');
console.log('wrote', outPath, 'bytes', Buffer.byteLength(js));
