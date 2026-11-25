# Установка YATV на Яндекс Станцию

Коротко: scattered скачать готовый архив под свою архитектуру, распаковать на колонке и запустить `install.sh`. Скрипт сам выберет способ автозапуска и **не перезапишет** ваши настройки.

## 1. Что понадобится

- Яндекс Станция с root-ADB (порт `5555`) — на ней уже включена отладка по сети;
- компьютер с `adb` (`platform-tools`) и доступом в ту же сеть;
- архив с GitHub Releases: https://github.com/vin2s/yatv/releases/latest
  - `yatv-armv7.tar` — для старых станций (32-bit ARM, `uname -m` = `armv7l`);
  - `yatv-aarch64.tar` — для новых (64-bit, `uname -m` = `aarch64`).

Узнать архитектуру:
```
adb connect 192.168.1.XXX:5555
adb shell uname -m
```

## 2. Установка (готовый архив)

```sh
adb connect 192.168.1.XXX:5555
adb push yatv-aarch64.tar /tmp/yatv.tar
adb shell "mkdir -p /tmp/yatv-pkg && tar -xf /tmp/yatv.tar -C /tmp/yatv-pkg"
adb shell "sh /tmp/yatv-pkg/yatv/install.sh"
```

Скрипт:
- скопирует `yatv`, `run.sh`, `yatv.env`, `S99yatv`, `VERSION` в `/data/opt/yatv`;
- сохранит `/data/opt/yatv.pre-update` (previous версия) — на случай отката;
- **не тронет** `config.json`, `planner.json`, `journal.jsonl`, `google-cache.json`;
- поставит автозапуск одним из способов:
  1. свой `/etc/init.d/S99yatv` — если каталог доступен для записи;
  2. подмена `/data/opt/soundya/run.sh` — если стоит immutable `S99soundya` (как на 218);
  3. один хук в `/data/alice-homebrew/start.sh` — если有其他 вариантов нет (как на 169);
- перезапустит демон.

## 3. Проверка

```
adb shell "ps | grep yatv"
```
Откройте в браузере: **http://192.168.1.XXX:8766/**

В панели должен быть блок «Обновление YATV» с версией.

## 4. Перенос настроек с другой колонки

Если на другой колонке уже всё настроено:
1. откройте её панель → «Перенос настроек» → «Скачать конфиг»;
2. на новой колонке: «Перенос настроек» → загрузите файл.

Перенесутся ТВ, фразы, планы, Google, настройки сводки. ⚠️ Файл содержит секреты Google — не публикуйте его.

## 5. Обновление

В панели «Обновление YATV»:
- «Проверить» — узнать версию;
- «Установить обновление» — скачать и поставить;
- «Обновляться автоматически» — колонка сама проверит GitHub раз в 6 часов и поставит новую версию.

## 6. Откат

Если новая версия не поднялась, `install.sh` оставляет бэкап:
```
adb shell "cp -a /data/opt/yatv.pre-update/. /data/opt/yatv/"
adb shell "/etc/init.d/S99yatv restart"
```

## 7. Сборка из исходников

```sh
sh scripts/build-arm.sh       # ARMv7  → dist/yatv-armv7.tar
sh scripts/build-aarch64.sh   # aarch64 → dist/yatv-aarch64.tar
```
Нужны WSL/Linux, Rust, кросс-компиляторы (см. скрипты). Сборка не требует компьютера на колонке.
