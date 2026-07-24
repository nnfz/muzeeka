const fs = require('fs');
const path = require('path');

const root = path.join(__dirname, '..');
const htmlPath = path.join(root, 'src-tauri', 'src', 'remote', 'index.html');
const snippetPath = path.join(__dirname, 'remote-icons-snippet.js');

let html = fs.readFileSync(htmlPath, 'utf8');
const snippet = fs.readFileSync(snippetPath, 'utf8');

const setIconRe =
  /    function setIcon\(el, name\) \{\n      if \(!el\) return;\n      const src = `\/icons\/\$\{name\}\.svg`;\n      if \(el\.getAttribute\('src'\) !== src\) el\.setAttribute\('src', src\);\n    \}\n/;

if (!setIconRe.test(html)) {
  // already patched or different content
  if (html.includes('ICONS_SVG')) {
    console.log('already has ICONS_SVG, re-patching from snippet');
    html = html.replace(
      /    \/\/ SVG icons inlined from static\/icons[\s\S]*?function iconSrc\(name\) \{\n[\s\S]*?\n    \}\n\n/,
      ''
    );
    html = html.replace(
      /    function setIcon\(el, name\) \{\n[\s\S]*?\n    \}\n\n    function setVolumeIcon/,
      '    function setVolumeIcon'
    );
  } else {
    console.error('Could not find setIcon block to replace');
    process.exit(1);
  }
} else {
  html = html.replace(setIconRe, '');
}

const insert = `${snippet}
    function setIcon(el, name) {
      if (!el) return;
      const src = iconSrc(name);
      if (src && el.getAttribute('src') !== src) el.setAttribute('src', src);
    }

`;

if (!html.includes('function setVolumeIcon')) {
  console.error('setVolumeIcon missing');
  process.exit(1);
}

html = html.replace(
  '    function setVolumeIcon',
  insert + '    function setVolumeIcon'
);

// img src="/icons/foo.svg" -> data-icon="foo" src=""
html = html.replace(/src="\/icons\/([a-z]+)\.svg"/g, 'data-icon="$1" src=""');

// init after setVolumeIcon
if (!html.includes("querySelectorAll('[data-icon]')")) {
  html = html.replace(
    /function setVolumeIcon\(vol\) \{\n      const v = Number\(vol\) \|\| 0;\n      const name = v <= 0 \? 'mute' : v > 0\.66 \? 'volmax' : v > 0\.33 \? 'volmed' : 'volmin';\n      setIcon\(\$\('iconVolume'\), name\);\n    \}/,
    (m) =>
      m +
      `

    document.querySelectorAll('[data-icon]').forEach((el) => {
      setIcon(el, el.dataset.icon);
    });`
  );
}

fs.writeFileSync(htmlPath, html, 'utf8');

console.log('patched', htmlPath);
console.log('size', html.length);
console.log('ICONS_SVG', html.includes('ICONS_SVG'));
console.log('/icons/ left', (html.match(/\/icons\//g) || []).length);
console.log('data-icon', (html.match(/data-icon=/g) || []).length);
