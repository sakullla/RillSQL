import { afterEach, describe, expect, it, vi } from "vitest";
import { effectScope, ref } from "vue";
import { useOutsideDismiss } from "./useOutsideDismiss";

function setup(initial: boolean, inside = false) {
  const open = ref(initial);
  const dismissed = vi.fn();
  const panel = document.createElement("div");
  document.body.append(panel);
  const scope = effectScope();
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
  document.body.innerHTML = "";
});

describe("useOutsideDismiss", () => {
  it("dismisses on a pointerdown outside the target", () => {
    const { open, dismissed, scope } = setup(true);
    document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    expect(dismissed).toHaveBeenCalledTimes(1);
    scope.stop();
  });

  it("does not dismiss when the pointer is inside the target", () => {
    const { dismissed, panel, scope } = setup(true, true);
    panel.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    expect(dismissed).not.toHaveBeenCalled();
    scope.stop();
  });

  it("ignores pointerdown while closed", () => {
    const { dismissed, scope } = setup(false);
    document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    expect(dismissed).not.toHaveBeenCalled();
    scope.stop();
  });

  it("stops listening after the scope stops", () => {
    const { open, dismissed, scope } = setup(true);
    scope.stop();
    open.value = false; // 不应抛错
    document.body.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
    expect(dismissed).not.toHaveBeenCalled();
  });
});
