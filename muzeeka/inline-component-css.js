// @ts-nocheck
import fs from 'node:fs';
import path from 'node:path';

const IMPORT_RE = /@import\s+['"](\.\/[^'"]+\.css)['"]\s*;?/g;

/** Inline sibling .css files so Vite watches them and Svelte HMR picks up style edits. */
export function inlineComponentCss() {
  return {
    name: 'inline-component-css',
    style: ({ content, filename }) => {
      if (!filename) return;

      const imports = [...content.matchAll(IMPORT_RE)];
      if (imports.length === 0) return;

      const dependencies = [];
      let code = content;

      for (const match of imports) {
        const cssPath = path.resolve(path.dirname(filename), match[1]);
        if (!fs.existsSync(cssPath)) continue;

        const css = fs.readFileSync(cssPath, 'utf-8');
        code = code.replace(match[0], css);
        dependencies.push(cssPath);
      }

      if (dependencies.length === 0) return;

      return { code, dependencies };
    },
  };
}

/** Forward .css edits to the .svelte file that @imports them (dev HMR). */
export function svelteScopedCssHmr() {
  return {
    name: 'svelte-scoped-css-hmr',
    handleHotUpdate({ file, server }) {
      if (!file.endsWith('.css')) return;

      const cssDir = path.dirname(file);
      const cssName = path.basename(file);
      const importNeedle = `./${cssName}`;
      const affected = [];

      for (const mod of server.moduleGraph.idToModuleMap.values()) {
        if (!mod.id?.endsWith('.svelte')) continue;
        if (path.dirname(mod.id) !== cssDir) continue;

        const source = fs.readFileSync(mod.id, 'utf-8');
        if (source.includes(importNeedle)) {
          affected.push(mod);
        }
      }

      return affected.length > 0 ? affected : undefined;
    },
  };
}