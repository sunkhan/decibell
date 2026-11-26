import { Fragment } from "./stub-react.js";
export { Fragment };
export const jsx = (type, props, key) => ({ type, props: props ?? {}, key });
export const jsxs = jsx;
export const jsxDEV = jsx;
