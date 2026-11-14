# rviewer

Minimálny fullscreen prehliadač obrázkov a videí pre Wayland. Žiadne GUI ani toolkit:
holý Wayland (xdg-shell) + EGL/OpenGL ES 2.0, obrázky cez crate `image`, video cez GStreamer (`playbin` → `appsink`, YUV→RGB v shaderi).
Správanie vychádza z mpv konfigurácie (`fs=yes`, `keep-open`, `image-display-duration=inf`, autoload, `video-unscaled=downscale-big`, drag-to-pan, cursor-centric zoom).

## Build (Debian 13)

    sudo apt install build-essential pkg-config libwayland-dev libxkbcommon-dev fontconfig fonts-dejavu-core \
        libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
        libegl1 libgles2 \
        gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-libav
    cargo build --release        # binárka: target/release/rviewer

Pre HW dekódovanie (VA-API) stačia `gstreamer1.0-plugins-bad` (plugin `va`) a ovládače VA.

## Použitie

    rviewer foto.jpg           # načíta celý adresár, začne na foto.jpg
    rviewer ~/Videá            # adresár
    rviewer a.png b.mp4 c.gif  # presne tieto súbory v tomto poradí
    rviewer --background=#202020 --sort=name-asc foto.jpg

Konfig: `~/.config/rviewer/rviewer.conf` (vzor `rviewer.conf`), skratky: `~/.config/rviewer/input.conf`, každú voľbu možno prepísať `--kľúč=hodnota`.

## Viac konfigurácií

    rviewer --config-dir=$HOME/.config/rviewer/slideshow foto.jpg

Načíta `ADRESÁR/rviewer.conf` a `ADRESÁR/input.conf` (ako `mpv --config-dir`). Samostatne sa dá zadať
`--config=SÚBOR` a `--input-conf=SÚBOR`, `--no-config` / `--no-input-conf` ich vypnú. Ľubovoľná voľba z konfigu sa dá
prepísať aj z príkazového riadku (`--background=#202020`, `--window`).

## Režim okno

`fullscreen=no` v konfigu alebo `--window`: okno má vždy veľkosť obrázka/videa (väčšie ako obrazovka sa zmenší) a mení sa
pri každom ďalšom súbore. Klávesa `f` (`toggle-fullscreen`) prepína medzi oknom a fullscreenom. Skutočný stav sa berie z kompozitora, takže funguje rovnako, aj keď fullscreen prepneš v kompozitore (napr. F11 vo Wayfire).
Wayland nedovoľuje klientovi okno premiestniť, takže **centrovanie bez skákania** zabezpečuje voliteľný
`window-place-command`: pri zmene obrázka sa spustí príkaz, ktorý kompozitoru nastaví veľkosť aj polohu naraz
(premenné `RVIEWER_WIDTH`, `RVIEWER_HEIGHT`, `RVIEWER_PID`; skript pre Wayfire je `contrib/wayfire-center.py`).
`rviewer` medzitým nechá na obrazovke posledný nakreslený snímok a nový nakreslí až po odpovedi (configure)
kompozitora; ak do 400 ms nepríde, zmení veľkosť sám. Staršie `window-center-command` okno až dodatočne presunie
(môže preskočiť). Bez oboch príkazov okno umiestni kompozitor podľa svojich pravidiel.
V tilingovom kompozitore veľkosť okna určuje kompozitor a obsah sa doň zmestí.

## OSD: info panel a vyhľadávacia lišta

Bez GUI toolkitu: text sa kreslí cez `fontdue` (atlas glyfov v GLES2), takže treba nejaký TrueType font
(nájde sa cez `fc-match`, alebo `osd-font=`).

- **Info panel** (`info`, predvolene pravé tlačidlo a `i`): vľavo hore názov, `[3/57]`, rozlíšenie, zoom, veľkosť súboru
  a pri videu čas, pauza a hlasitosť. Je trvalý, kým ho znova neprepneš.
- **Vyhľadávacia lišta videa**: dole, zobrazí sa pri pohybe myši, pauze alebo posune a po `seekbar-timeout` zmizne
  (ak je myš nad lištou, ostane). Klik alebo ťah ľavým tlačidlom po dráhe = posun vo videu (pri ťahaní rýchly na keyframe,
  po pustení presný). Pri pauze sa vľavo zobrazí ikona pauzy. Vypnúť sa dá `seekbar=no`.

## Odstránenie vstavanej skratky

Súbor `input.conf` vstavané skratky len dopĺňa a prepisuje. Skratku odstrániš riadkom `SPACE ignore`, alebo nastavíš
`default-bindings=no` a v `input.conf` uvedieš všetky skratky, ktoré chceš (vzor je `input.conf` v repozitári).

## Slideshow

- Medzerník pri obrázku (alebo `s`, príkaz `slideshow`) spustí/zastaví slideshow; pri videu medzerník ďalej pauzuje.
- `slideshow-delay=5` je dĺžka zobrazenia obrázka v sekundách, video sa prehrá celé a potom príde ďalší súbor.
  Ručné `next`/`prev` časovač obnoví. `slideshow-loop=yes` po poslednom súbore skočí na prvý.
- Od štartu: `slideshow=yes` v konfigu alebo `rviewer --slideshow --slideshow-delay=3 ...`.

## Titulky

- Externé: pri videu sa v jeho adresári hľadá `film.srt`, `film.sk.srt`, `film.en.forced.ass` (prípony srt, ass, ssa, vtt, sub, smi);
  vyberie sa podľa `sub-languages`, potom súbor bez jazyka.
- Vložené stopy v kontajneri (mkv, mp4) sa zobrazia automaticky.
- `j` (`cycle-sub`) prepína stopy a nakoniec vypne titulky, `z` / `x` (`sub-delay -0.1` / `0.1`) posúvajú načasovanie.
  Správa sa zobrazí v OSD, stav je aj v info paneli. `subtitles=no` ich vypne úplne.
- Staré `.srt` v CP1250: `sub-encoding=CP1250`. Veľkosť a vzhľad: `sub-font=Sans Bold 24`.
- Treba pluginy `gstreamer1.0-plugins-base` (textoverlay, subparse) a `gstreamer1.0-plugins-bad` (assrender pre ASS/SSA).

## Nezobraziteľné súbory

`skip-broken=yes` (predvolené): súbor, ktorý sa nepodarí dekódovať alebo prehrať (napr. text s príponou `.jpg`), sa preskočí
a vyradí zo zoznamu, takže sa k nemu nevrátiš ani pri listovaní naspäť. Ide sa smerom, ktorým si listoval. Ak nezostane žiadny
zobraziteľný súbor, program skončí. Pri videu sa za chybné považuje súbor, ktorý zlyhá skôr, než príde prvý snímok.

## Zobrazenie

- Obrázok v skutočnej veľkosti; ak je väčší ako obrazovka, zmenší sa (mipmapy, aby nebol aliasing).
- Video: ako obrázok; `video-min-size=1280x720` zväčší menšie video, `video-max-size=50%` (percentá obrazovky ako `autofit` v mpv; aj `1280x720`, `50%x40%`) zmenší väčšie; väčšie ako obrazovka sa vždy zmenší.
- `video-fit=fit` (alebo klávesa `v` / `cycle-fit`): vo **fullscreene** sa video zväčší alebo zmenší na celú obrazovku so zachovaním pomeru strán; `real` je skutočná veľkosť. V režime okno sa `fit` nepoužije a obrázkov sa netýka.
- Kurzor sa skrýva po `cursor-autohide` ms (0 = nikdy). Ak kompozitor nemá `cursor-shape-v1`, použije sa xcursor téma (`XCURSOR_THEME`, `XCURSOR_SIZE`).
- `background`: `#RRGGBB` alebo `#AARRGGBB` (ako v mpv).
- Na konci videa zostane posledný snímok (keep-open); Space potom pustí video odznova.
- GIF sa animuje; EXIF otočenie a rotácia videa podľa metadát sa aplikujú.

## Ovládanie a skratky

Všetky skratky sa dajú zmeniť v `~/.config/rviewer/input.conf` (alebo `--input-conf=SÚBOR`), formát ako mpv `input.conf`:

    Ctrl+WHEEL_UP  zoom 0.1
    x              quit
    ESC            ignore        # zruší predvolenú skratku

Vlastný súbor prepíše len riadky, ktoré obsahuje; ostatné ostanú predvolené (zoznam príkazov a názvov kláves je v `input.conf` z repozitára, ktorý je zároveň predvolená sada).
Znaky sa berú podľa aktuálneho rozloženia klávesnice (cez xkbcommon), takže `Ctrl+=` je kláves, na ktorom máš `=`; pomenované klávesy (`ESC`, `LEFT`, `F5`, `KP_PLUS`...) sú fyzické. Modifikátory: `Ctrl+`, `Shift+`, `Alt+`.

Predvolené:

| Vstup | Príkaz |
|---|---|
| koliesko hore/dole | `prev` / `next` |
| ľavé tlačidlo + ťah | `drag-pan` (len ak je obsah väčší ako obrazovka) |
| stredné tlačidlo, Space | `pause` |
| pravé tlačidlo, `i` | `info` (zapne/vypne info panel) |
| Ctrl+`=` / Ctrl+`-`, KP +/− | `zoom 0.1` / `zoom -0.1` (okolo kurzora, nezmenší pod celý obsah) |
| `r`, Ctrl+`0` | `zoom-reset` |
| ←/→ | `seek-or-step ∓5` (video: posun, obrázok: predošlý/ďalší) |
| ↑/↓ | `seek ±60` |
| PgUp/PgDn, Enter, Home/End | `prev`/`next`, `first`/`last` |
| `f`, `F11` | `toggle-fullscreen` (okno ↔ fullscreen) |
| `v` | `cycle-fit` (video: skutočná veľkosť ↔ fit na obrazovku) |
| `j`, `z` / `x` | `cycle-sub`, `sub-delay -0.1` / `0.1` (titulky) |
| `m`, `9`, `0` | `mute`, `volume -2` / `volume 2` |
| Esc, `q` | `quit` |

## Čo nie je (zámerne / zatiaľ)

Žiadny ovládací panel s tlačidlami (len vyhľadávacia lišta), galéria ani prechodové efekty. Titulky sa vykresľujú v rozlíšení videa (nie ostro v rozlíšení obrazovky), bez vlastného štýlovania a bez bitmapových titulkov z DVD.
Zväčšenie je bilineárne (alebo `scale=nearest`), nie spline36. Mierka displeja je len celočíselná.
