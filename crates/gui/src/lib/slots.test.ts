// "Room slots": the same arithmetic as in the core (`look::room_capacity`), and the load hint.
import { describe, expect, it } from "vitest";
import { DEFAULT_LOOK, roomCapacity, SLOTS, slotsLoad, slotsWord } from "./core";

describe("места в комнате", () => {
  it("по умолчанию 20, от 2 до 50", () => {
    expect(DEFAULT_LOOK.slots).toBe(20);
    expect(SLOTS).toMatchObject({ min: 2, max: 50, default: 20, vanilla: 8 });
  });

  it("без Phase Companion игра пускает не больше восьми", () => {
    expect(roomCapacity(20, false)).toBe(8);
    expect(roomCapacity(50, false)).toBe(8);
    expect(roomCapacity(4, false)).toBe(4);
  });

  it("с Phase Companion — сколько задал хост, в пределах", () => {
    expect(roomCapacity(20, true)).toBe(20);
    expect(roomCapacity(50, true)).toBe(50);
    expect(roomCapacity(80, true)).toBe(50);
    expect(roomCapacity(1, true)).toBe(2);
  });

  it("подсказка о нагрузке по порогам 10 и 20", () => {
    expect([2, 10].map(slotsLoad)).toEqual(["light", "light"]);
    expect([11, 20].map(slotsLoad)).toEqual(["normal", "normal"]);
    expect([21, 50].map(slotsLoad)).toEqual(["heavy", "heavy"]);
  });

  it("склонение", () => {

    expect([1, 2, 5, 11, 21, 22, 50].map(slotsWord)).toEqual([
      "1 место",
      "2 места",
      "5 мест",
      "11 мест",
      "21 место",
      "22 места",
      "50 мест",
    ]);
  });
});
