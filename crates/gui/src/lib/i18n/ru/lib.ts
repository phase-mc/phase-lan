// Русский словарь: общие слова, форматы, журнал и сторы (src/lib). Русский — источник
// ключей: остальные языки обязаны повторить их все.

export const lib = {
  // ---- вкладки
  "nav.host": "Хост",
  "nav.join": "Подключиться",
  "nav.mods": "Моды",
  "nav.journal": "Журнал",
  "nav.network": "Сеть",
  "nav.networkFull": "Сеть и relay",
  "nav.look": "Оформление",
  "nav.lookFull": "Оформление комнаты",
  "nav.settings": "Настройки",

  // ---- числа и размеры
  "size.mb": "{n} МБ",
  "size.kb": "{n} КБ",
  "count.mods": {
    one: "{n} мод",
    few: "{n} мода",
    many: "{n} модов",
    other: "{n} модов",
  },
  "count.slots": {
    one: "{n} место",
    few: "{n} места",
    many: "{n} мест",
    other: "{n} мест",
  },
  "count.players": {
    one: "{n} игрок",
    few: "{n} игрока",
    many: "{n} игроков",
    other: "{n} игрока",
  },

  // ---- проверки ввода
  "prefix.tooLong": "Не длиннее {max} знаков. Сейчас {len}.",
  "prefix.noSpaces": "Без пробелов.",
  "prefix.badChar": "Знак «{ch}» недопустим: буквы, цифры и - _ . ! + * # &.",
  "nickname.empty": "Ник не может быть пустым.",
  "nickname.tooLong": "Ник — не длиннее {max} знаков. Сейчас {len}.",
  "profile.nameRule": "Только латинские буквы, цифры, «-» и «_». До 32 знаков.",
  "settings.portError":
    "Порт — число от 1 до 65535. Игра пишет его в чате после «Открыть для сети».",

  // ---- сеть
  "network.local": "Только этот ПК",
  "network.relayCustom": "свой relay",
  "network.relayN0": "relay n0",
  "network.relayOnly": "Только {relay}",
  "network.direct": "Напрямую · {relay}",

  // ---- пути связи
  "path.direct": "напрямую",
  "path.relay": "через relay",
  "path.other": "по запасному пути",

  // ---- сторы
  "app.sessionFailed": "Сессия завершилась с ошибкой.",
  "look.noConnection": "Подключения сейчас нет.",
  "modsDir.runningGame": "Из запущенной игры",
  "modsDir.chosen": "Выбрана вручную",
  "modsDir.default": "Стандартная",
  "modsRestore.done": "Ваши моды возвращены: {restored}. Убрано модов хоста: {removed}.",
  "modsRestore.conflicts": {
    one: "{n} мод остался в бэкапе: в папке уже есть файл с таким именем.",
    few: "{n} мода остались в бэкапе: в папке уже есть файлы с такими именами.",
    many: "{n} модов остались в бэкапе: в папке уже есть файлы с такими именами.",
    other: "{n} мода остались в бэкапе: в папке уже есть файлы с такими именами.",
  },
  "modsRestore.failed": {
    one: "Занят {n} файл. Закройте Minecraft и повторите.",
    few: "Заняты {n} файла. Закройте Minecraft и повторите.",
    many: "Заняты {n} файлов. Закройте Minecraft и повторите.",
    other: "Заняты {n} файла. Закройте Minecraft и повторите.",
  },

  // ---- журнал
  "journal.access.allowed": "Игрок {who} получил доступ. Ответ запомнен.",
  "journal.access.denied": "Вы отклонили игрока {who}.",
  "journal.access.alreadyDenied": "Игрок {who} снова просит доступ. Он уже отклонён.",
  "journal.access.promptsOff": "Игрок {who} отклонён: новые устройства не принимаются.",
  "journal.access.busy": "Игрок {who} отклонён: вы ещё отвечаете на другой запрос.",
  "journal.access.timeout": "Вы не ответили вовремя. Игрок {who} отклонён.",
  "journal.access.guestLeft": "Игрок {who} ушёл, не дождавшись ответа.",
  "journal.access.roomFull": "Игрок {who} не вошёл: в комнате нет свободных мест.",
  "journal.joinedVia": "Игрок {who} подключился {path}.",
  "journal.joined": "Игрок {who} подключился.",
  "journal.introduced": "Игрок {id} представился: {nickname}.",
  "journal.gameName": "Игрок {who} входит в мир под ником {name}.",
  "journal.disconnected": "Игрок {who} отключился.",
  "journal.pathGuest": "Связь с хостом идёт {path}.",
  "journal.pathHost": "Игрок {who}: связь идёт {path}.",
  "journal.gameUnreachable": "Игра на порту {port} не принимает подключение. Мир открыт для сети?",
  "journal.hostLost": "Связь с хостом потеряна. Переподключаюсь.",
  "journal.hostRestored": "Связь с хостом восстановлена.",
  "journal.voice": "Голос: отправлено {sent}, получено {received}, потеряно {dropped} пакетов.",
  "journal.worldSearch": "Ищу открытый LAN-мир на этом ПК (до {s} с).",
  "journal.worldFound": "Найден мир «{name}» на порту {port} ({summary}).",
  "journal.worldMovedFollowed":
    "Мир «{name}» снова открыт — теперь на порту {to}. Комната та же: друзья войдут по прежнему коду.",
  "journal.worldMovedManual":
    "Мир «{name}» открыт на порту {to}, а комната ведёт на порт {from}, указанный вручную. Выберите «Найти сам» или порт {to} и создайте комнату заново.",
  "journal.gameChecked": "Порт {port}: {summary}.",
  "journal.approvalRequested": "Игрок {who} просит войти в комнату.",
  "journal.reportSaved": "Отчёт сессии сохранён: {path}",
  "journal.host.open": "Комната открыта. Код для друзей готов.",
  "journal.host.world": "Мир «{motd}»: подключения друзей идут в игру на порт {port}.",
  "journal.host.noPort": "Порт игры не задан: работает только проверка канала.",
  "journal.host.voiceOff": "Голос (UDP) не пересылается.",
  "journal.host.voiceNone": "Голос (UDP): подходящих портов нет.",
  "journal.host.voicePorts": "Голос (UDP): порты {ports}.",
  "journal.host.loopback": "Режим «только этот ПК»: гость — на этом же компьютере.",
  "journal.host.relayFree": "Relay: бесплатные серверы n0. Они только для проверок.",
  "journal.host.relayCustom": "Relay: {urls}.",
  "journal.guest.connectedHost": "Подключено к миру «{motd}» игрока {host} ({id}).",
  "journal.guest.connected": "Подключено к миру «{motd}».",
  "journal.guest.lan":
    "Мир объявлен в «Локальной сети» Minecraft как «{lan}». Адрес: {addr}.",
  "journal.guest.addr": "Адрес для Minecraft: {addr}.",
  "journal.guest.voice": "Голос (UDP): открыты порты {ports} на 127.0.0.1.",
} as const;
