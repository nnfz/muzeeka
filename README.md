# Muzeeka

Tauri + SvelteKit desktop audio player.

## Build types

Два вида сборки.

### Installer

```powershell
npm run build:installer
# или с флагами
npm run tauri -- build --bundles nsis
```

Готовый инсталлер лежит в:
`src-tauri/target/release/bundle/nsis/`

### Portable

```powershell
npm run build:portable
# или
npm run tauri -- build --no-bundle
```

После сборки руками:
1. Берёшь `src-tauri/target/release/muzeeka.exe`
2. Копируешь рядом папку `src-tauri/bass/`
3. Пакуешь в zip — готово.

### Оба сразу

```powershell
npm run build:both
```

Посмотреть доступные команды:
```powershell
npm run
```

## Development

```powershell
npm install
npm run tauri dev
```

## Available scripts (npm run)

- `dev`, `build`, `check` — фронтенд
- `tauri` / `tauri dev` / `tauri build` — напрямую Tauri CLI
- `build:installer`
- `build:portable`
- `build:both`

## Recommended IDE Setup

[VS Code](https://code.visualstudio.com/) + [Svelte](https://marketplace.visualstudio.com/items?itemName=svelte.svelte-vscode) + [Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) + [rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer).

## Notes

- Для портейбла обязательно нужна папка `bass/` рядом с `muzeeka.exe`.
- На целевой машине обычно требуется **WebView2** (и иногда Visual C++ Redistributable).
