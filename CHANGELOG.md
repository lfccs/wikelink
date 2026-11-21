# Changelog

## 0.1.1

- **Overlay opens over the game again.** After a recent Windows update, `Shift+Tab` could end in "could not open over the game" every time: Windows refused to bring the overlay to the front because the key press had gone to the game. When that happens the overlay now sends one empty input event (a mouse event with no movement and no button) and asks again. It still never synthesizes a key press, a click or a mouse movement, and never touches the game process.
- **`overlay.log`.** The overlay notes, in `%LOCALAPPDATA%\wikelo-sc\data\overlay.log`, what Windows answered each time it was asked to open: the window in front before and after, and the overlay's own window. The file stays on your PC and helps when reporting a problem.
- **Game data is not sent again just because Wikelink has a new version.** A copy the site already has is replaced only when this version extracts more than the one that made it.

### Português

- **O overlay volta a abrir sobre o jogo.** Depois de uma atualização recente do Windows, o `Shift+Tab` podia terminar sempre em "não foi possível abrir sobre o jogo": o Windows recusava trazer o overlay para a frente porque a tecla tinha ido para o jogo. Quando isso acontece, o overlay agora envia um único evento de entrada vazio (mouse sem movimento e sem botão) e pede de novo. Ele continua sem sintetizar tecla, clique ou movimento de mouse, e sem tocar no processo do jogo.
- **`overlay.log`.** O overlay anota, em `%LOCALAPPDATA%\wikelo-sc\data\overlay.log`, o que o Windows respondeu a cada pedido de abrir: a janela que estava na frente antes e depois, e a janela do próprio overlay. O arquivo fica no seu PC e ajuda na hora de relatar um problema.
- **Os dados do jogo não são reenviados só porque o Wikelink mudou de versão.** Uma cópia que o site já tem só é substituída quando esta versão extrai mais do que a que a gerou.

## 0.1.0

First release under the name Wikelink (before: *Wikelist Watcher*, `wikelo-watcher.exe`).
