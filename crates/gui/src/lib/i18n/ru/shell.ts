// Русский словарь: каркас окна и общие компоненты (верхняя строка, ошибки, диалоги ссылок).

export const shell = {
  // ---- верхняя строка
  "topbar.sections": "Разделы",
  "topbar.window": "Окно",
  "topbar.minimize": "Свернуть",
  "topbar.restore": "Восстановить",
  "topbar.maximize": "Развернуть",
  "topbar.close": "Закрыть",

  // ---- общее
  "count.steps": {
    one: "{n} шаг",
    few: "{n} шага",
    many: "{n} шагов",
    other: "{n} шага",
  },
  "help.howToJoin": "Как подключить мир?",
  "help.voiceChat": "Голосовой чат",
  "error.title": "Ошибка",
  "error.profile": "Профиль «{name}»: {error}",
  "error.dismiss": "Скрыть ошибку",
  "copy.label": "Скопировать",
  "copy.done": "Скопировано",
  "copy.failed": "Не удалось",
  "client.title": "Phase Client — скоро",
  "client.text": "Свой запуск игры, косметика и друзья. Phase LAN будет внутри.",
  "client.dismiss": "Скрыть анонс",
  "device.idLabel": "ID устройства: {id}",
  "preview.spoken": "В игре: {line}",
  "preview.spokenGlow": "В игре: {line}, силуэт подсвечен",

  // ---- ссылка phase://
  "link.label": "Приглашение",
  "link.title": "Отключиться и перейти?",
  "link.room": "Пришла ссылка на комнату",
  "link.invite": "Пришла ссылка с приглашением в другую комнату.",
  "link.closesRoom": "Ваша комната закроется, гости отключатся.",
  "link.endsConnection": "Текущее подключение закончится.",
  "link.fillCode": "Код встанет на вкладке «Подключиться», а подключитесь вы сами.",
  "link.fillInvite": "Приглашение встанет на вкладке «Подключиться», а подключитесь вы сами.",
  "link.stay": "Остаться",
  "link.go": "Перейти",

  // ---- вход без проверки аккаунта
  "account.toggle": "Пускать игроков без проверки аккаунта",
  "account.hint":
    "Скины игроков с лицензией в таком режиме у других не грузятся. Ник закрепляется за устройством, которое первым вошло с ним. Нужен Phase Companion в игре хоста.",

  // ---- Phase Companion
  "companion.text":
    "Шапка Tab, цветные ники, больше 8 мест. Бесплатно, ставится в игру хоста, гостям ничего не нужно.",
  "companion.install": "Поставить",
  "companion.installing": "Ставим…",
  "companion.doneRestart": "Готово. Перезапустите Minecraft один раз, чтобы мод заработал.",
  "companion.doneNext": "Готово. Мод заработает при следующем запуске игры.",
  "companion.upToDate": "Phase Companion уже стоит, обновлений нет.",
  "companion.noBuild": "Для этой версии игры мода пока нет. Мы добавляем версии по очереди.",
  "companion.unknownLoader":
    "Не нашли Fabric, Forge или NeoForge в этой сборке. Запустите игру с одним из них.",
  "companion.unknownGame": "Откройте мир в Minecraft: Phase увидит версию игры и подберёт мод.",
  "companion.noModsFolder":
    "Папка модов не найдена. Выберите её в карточке синхронизации модов.",
} as const;
