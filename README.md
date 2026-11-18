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
Wayland nedovoľuje klientovi okno premiestniť, preto sa **centrovanie bez skákania** rieši cez kompozitor:

- **Wayfire (predvolene, `wayfire-ipc=yes`)**: rviewer sa pripojí na IPC socket (`WAYFIRE_SOCKET`) a sám okno vycentruje,
  externý skript netreba. Pri prvom zobrazení odoberá udalosť `view-pre-map`: Wayfire okno podrží, rviewer ho
  vycentruje (v pracovnej oblasti výstupu bez panelov) a zavolá `unblock-map`, takže okno vznikne rovno na správnom
  mieste. Pri zmene obrázka/videa nastaví veľkosť aj polohu jedným `configure-view`. Mimo Wayfire sa nič nedeje.
- **Iné kompozitory**: voliteľný `window-place-command` spustí príkaz, ktorý kompozitoru nastaví veľkosť aj polohu naraz
  (premenné `RVIEWER_WIDTH`, `RVIEWER_HEIGHT`, `RVIEWER_PID`; starší skript pre Wayfire je `contrib/wayfire-center.py`).
  Príkaz má prednosť pred `wayfire-ipc`. Staršie `window-center-command` okno až dodatočne presunie (môže preskočiť).

`rviewer` medzitým nechá na obrazovke posledný nakreslený snímok a nový nakreslí až po odpovedi (configure)
kompozitora; ak do 400 ms nepríde, zmení veľkosť sám. Bez týchto nastavení okno umiestni kompozitor podľa svojich pravidiel.
V tilingovom kompozitore veľkosť okna určuje kompozitor a obsah sa doň zmestí.

## OSD: info panel a vyhľadávacia lišta

Bez GUI toolkitu: text sa kreslí cez `fontdue` (atlas glyfov v GLES2), takže treba nejaký TrueType font
(nájde sa cez `fc-match`, alebo `osd-font=`).

- **Info panel** (`info`, predvolene pravé tlačidlo a `i`): vľavo hore názov, `[3/57]`, rozlíšenie, zoom, veľkosť súboru
  a pri videu čas, pauza a hlasitosť. Je trvalý, kým ho znova neprepneš.
- **Vyhľadávacia lišta videa**: dole, zobrazí sa pri pohybe myši, pauze alebo posune a po `seekbar-timeout` zmizne
  (ak je myš nad lištou, ostane). Klik alebo ťah ľavým tlačidlom po dráhe = posun vo videu (pri ťahaní rýchly na keyframe,
  po pustení presný). Pri pauze sa vľavo zobrazí ikona pauzy. Vypnúť sa dá `seekbar=no`.

## Rýchle listovanie

`preload=yes` na pozadí dopredu načíta susedný obrázok v smere listovania (aj predchádzajúci). Prepnutie je potom okamžité.
Animované GIF-y sa nepredčítavajú. Stojí to pamäť pre dva dekódované obrázky; vypnutie: `preload=no`.

## Odstránenie vstavanej skratky

Súbor `input.conf` vstavané skratky len dopĺňa a prepisuje. Skratku odstrániš riadkom `SPACE ignore`, alebo nastavíš
`default-bindings=no` a v `input.conf` uvedieš všetky skratky, ktoré chceš (vzor je `input.conf` v repozitári).

## Streamy z internetu

`rviewer URL` prehrá priamy odkaz na video (`https://…/film.mp4`), HLS/DASH (`.m3u8`, `.mpd`) a prúdy `rtsp://`, `rtmp://` a pod.
Adresy sa prehrávajú priamo cez GStreamer, bez `yt-dlp`, takže začnú hrať hneď. Stránky typu YouTube (adresa nie je video, ale HTML) tak nefungujú;
na ne použi mpv. Adresy a súbory sa dajú miešať v jednom zozname: `rviewer a.mp4 https://…`.

- Pri nízkom bufferi sa prehrávanie videa samo pozastaví a v info paneli je `načítavam: N %`. Buffer je `net-buffer=3` s (rádiá a živé prúdy bez trvania sa kvôli bufferu nepozastavujú).
- Živé prenosy nemajú trvanie ani vyhľadávaciu lištu. Náhľady na lište a externé titulky sa pri adresách nepoužívajú.
- Obrázky z internetu zatiaľ nie.
- Treba `gstreamer1.0-plugins-good` (HTTP), `gstreamer1.0-plugins-bad` (HLS/DASH), `glib-networking` (HTTPS).

## Playlist (`.m3u`, `.m3u8`, `.pls`)

`rviewer ~/Dokumenty/tv.m3u` načíta zoznam staníc alebo súborov (aj spolu s ďalšími argumentmi). Názvy sa berú z `#EXTINF`
(a z `tvg-name`), relatívne cesty od adresára playlistu, riadky `#EXTVLCOPT:http-referrer=` a `http-user-agent=` aj atribút `http-referrer="…"`
sa pošlú ako hlavičky pri otváraní adresy (pri HLS sa môžu uplatniť len na prvý dotaz, nie na jednotlivé segmenty).
Lokálny `.m3u8` s `#EXT-X-` (HLS súbor, nie zoznam) sa berie ako video. Zoznam z internetu (`http://…/zoznam.m3u`) zatiaľ nie, stiahni ho.

Skratka `l` (príkaz `playlist`) otvorí zoznam v strede obrazovky: šípky a PgUp/PgDn/Home/End vyberajú, `Enter` prehrá, `Esc` alebo `l` zavrie,
kolieskom myši sa posúva a kliknutím sa vybraná položka prehrá. Zoznam funguje pre ľubovoľný zoznam súborov (aj priečinok s fotkami).
V `--nogui` vypíše zoznam do terminála.

## Bez okna (`--nogui`)

`rviewer --nogui https://play.radio7.sk/128` (alebo `--no-window`) nevytvorí žiadne okno a ani sa nepripája k Waylandu. Prehráva len zvuk,
začne hneď a funguje aj bez grafickej relácie. Zoznam môže obsahovať súbory aj adresy; po skončení skladby ide ďalšia a po poslednej
sa program ukončí. Názov skladby z tagov prúdu (rádio) sa vypíše ako `♪ …`.

Ovládanie z terminála podľa `input.conf`: `q` koniec, medzerník pauza, `9`/`0` hlasitosť, šípky pretáčanie, `Enter`/`PgDn` ďalšia, `PgUp` predchádzajúca,
`i` stav, Ctrl+C koniec. Bez terminála sa ukončí cez `pkill rviewer`.

## Zvuk bez okna (rádiá, hudba)

Ak prúd nemá obraz (rádio, mp3, ...), okno sa vôbec nezobrazí a rviewer beží na pozadí ako mpv bez okna. Okno sa neukáže ani na chvíľu,
lebo sa s mapovaním čaká na prvý snímok videa (najviac 1,5 s pri súbore, 4 s pri sieti, potom sa zobrazí aj prázdne, aby chyba nezostala skrytá).

- Ak je rviewer spustený v termináli a je jeho popredným procesom, ovláda sa z neho: znaky a medzerník podľa `input.conf`
  (`q` koniec, medzerník pauza, `9`/`0` hlasitosť, šípky...), Ctrl+C ukončí.
- Inak sa ukončí cez `Ctrl+C`, `kill`/`pkill rviewer`; SIGINT, SIGTERM a SIGHUP ukončia program čisto.
- Správy (hlasitosť, titulky...) idú na stderr.

## Vlastné príkazy (`run`)

V `input.conf` môžeš skratke priradiť shellový príkaz: `run PRÍKAZ` ho spustí na pozadí, `run-remove PRÍKAZ` ho spustí
a súbor zároveň zmizne zo zoznamu (po presune do koša, premenovaní...). V príkaze fungujú `{path}`, `{dir}`, `{name}` a `{stem}`
(vložia sa správne ošetrené, takže medzery a úvodzovky v názve nevadia); cesta je aj v premennej `RVIEWER_PATH`.

```
DEL run-remove gio trash {path}
e   run gimp {path}
c   run sh -c 'printf %s {path} | wl-copy'
```

Pozor: `run-remove` zoznam upraví vždy, aj keď príkaz zlyhá. Riadok v `input.conf` nesmie obsahovať ` #` (považuje sa za komentár).

## Náhľady na lište

Pri prechode myšou po vyhľadávacej lište (a pri ťahaní po nej) sa nad ňou ukáže náhľad snímku s časom, podobne ako `thumbfast`.
Vytvára ich druhý pipeline GStreameru (pozastavený, rýchly seek na kľúčový snímok, malý RGBA obraz), ktorý sa spustí
raz pri prvom prechode myšou po lište. Vypnutie: `thumbnails=no`, veľkosť: `thumbnail-size=200`.

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
| pravé tlačidlo (držať) + ťah | `peek` (ako FastStone: kým držíš, obrázok/video v 100 % a posúvaš ho myšou; po pustení sa vráti pôvodný pohľad) |
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
| `l` | `playlist` (zoznam súborov/staníc na výber) |
| `j`, `z` / `x` | `cycle-sub`, `sub-delay -0.1` / `0.1` (titulky) |
| `[` `]` / `<` `>`, Backspace | `speed 0.9` / `1.1` (rýchlosť prehrávania, výška tónu sa zachová), `speed-reset` |
| `,` / `.` | `frame-back-step` / `frame-step` (po snímkoch, video sa pozastaví) |
| `Ctrl+l`, `a` | `loop-file` (opakovanie videa, aj `loop-file=yes`), `cycle-audio` (zvuková stopa) |
| `m`, `9`, `0` | `mute`, `volume -2` / `volume 2` |
| Esc, `q` | `quit` |

## Doostrenie

`sharpen=0..1` v `rviewer.conf` (predvolene 0 = vypnuté) doostrí obraz len pri zmenšení (unsharp mask v shaderi; pri videu len jas).
Hodnoty okolo 0.3–0.6 dajú vzhľad podobný swayimg/mpv.

## Čo nie je (zámerne / zatiaľ)

Žiadny ovládací panel s tlačidlami (len vyhľadávacia lišta), galéria ani prechodové efekty. Titulky sa vykresľujú v rozlíšení videa (nie ostro v rozlíšení obrazovky), bez vlastného štýlovania a bez bitmapových titulkov z DVD.
Zväčšenie je bilineárne (alebo `scale=nearest`), nie spline36. Mierka displeja je len celočíselná.
