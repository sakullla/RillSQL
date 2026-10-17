import { afterEach, describe, expect, it, vi } from "vitest";
import { effectScope, nextTick, ref, type EffectScope } from "vue";
import { useOutsideDismiss } from "./useOutsideDismiss";

const scopes: EffectScope[] = [];

function setup(initial: boolean, inside = false) {
  const open = ref(initial);
  const dismissed = vi.fn();
  const panel = document.createElement("div");
  document.body.append(panel);
  const scope = effectScope();
  scopes.push(scope);
  scope.run(() =>
    useOutsideDismiss(
      open,
      (target) => inside && panel.contains(target),
      dismissed,
    ),
  );
  return { open, dismissed, panel, scope };
}

afterEach(() => {
  vi.restoreAllMocks();
  for (const scope of scopes) scope.stop();
  scopes.length = 0;
  document.body.innerHTML = "";
});

describe("useOutsideDismiss", () => {
  it("dismisses on a pointerdown outside the target", () => {
    const { dismissed } = setup(true);
    document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    expect(dismissed).toHaveBeenCalledTimes(1);
  });

  it("does not dismiss when the pointer is inside the target", () => {
    const { dismissed, panel } = setup(true, true);
    panel.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    expect(dismissed).not.toHaveBeenCalled();
  });

  it("ignores pointerdown while closed", () => {
    const { dismissed } = setup(false);
    document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    expect(dismissed).not.toHaveBeenCalled();
  });

  it("stops listening after the scope stops", () => {
    const { open, dismissed, scope } = setup(true);
    scope.stop();
    open.value = false; // 不应抛错
    document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    expect(dismissed).not.toHaveBeenCalled();
  });

  it("attaches the listener in the capture phase", () => {
    const addSpy = vi.spyOn(document, "addEventListener");
    setup(true);
    const call = addSpy.mock.calls.find(([type]) => type === "pointerdown");
    expect(call?.[2]).toBe(true);
  });

  it("starts listening when open flips to true", async () => {
    const { open, dismissed } = setup(false);
    document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    expect(dismissed).not.toHaveBeenCalled();
    open.value = true;
    await nextTick();
    document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    expect(dismissed).toHaveBeenCalledTimes(1);
  });

  it("stops listening when open flips to false", async () => {
    const { open, dismissed } = setup(true);
    open.value = false;
    await nextTick();
    document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    expect(dismissed).not.toHaveBeenCalled();
  });
});
