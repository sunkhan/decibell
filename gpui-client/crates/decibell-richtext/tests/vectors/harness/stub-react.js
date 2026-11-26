// React stand-in: hooks run inline once, so a component called as a plain function returns the
// element tree it would render on its first commit.
export const useState = (init) => [typeof init === "function" ? init() : init, () => {}];
export const useCallback = (fn) => fn;
export const useMemo = (fn) => fn();
export const useRef = (current) => ({ current });
export const useEffect = () => {};
export const useLayoutEffect = () => {};
export const useImperativeHandle = () => {};
export const useDeferredValue = (v) => v;
export const memo = (c) => c;
export const forwardRef = (render) => (props) => render(props, null);
export const Fragment = Symbol.for("stub.fragment");
export default { useState, useCallback, useMemo, useRef, useEffect, useLayoutEffect, useImperativeHandle, useDeferredValue, memo, forwardRef, Fragment };
