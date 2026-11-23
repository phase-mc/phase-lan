import { describe, expect, it } from "vitest";
import { sceneMode } from "./sceneMode";

describe("sceneMode", () => {
  it("в фокусе — полная частота, игра не важна", () => {
    expect(sceneMode({ visible: true, focused: true, gameRunning: false })).toBe("full");
    expect(sceneMode({ visible: true, focused: true, gameRunning: true })).toBe("full");
  });

  it("без фокуса и без игры — 30 к/с, под курсором — полная", () => {
    expect(sceneMode({ visible: true, focused: false, gameRunning: false })).toBe("calm");
    expect(sceneMode({ visible: true, focused: false, awake: true, gameRunning: false })).toBe("full");
  });

  it("без фокуса при запущенной игре — 0 к/с, даже под курсором", () => {
    expect(sceneMode({ visible: true, focused: false, gameRunning: true })).toBe("off");
    expect(sceneMode({ visible: true, focused: false, awake: true, gameRunning: true })).toBe("off");
  });

  it("скрыто или свёрнуто — 0 к/с при любых остальных входах", () => {
    for (const focused of [true, false])
      for (const awake of [true, false])
        for (const gameRunning of [true, false])
          expect(sceneMode({ visible: false, focused, awake, gameRunning })).toBe("off");
  });
});
