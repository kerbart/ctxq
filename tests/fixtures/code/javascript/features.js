import thing from "./thing.js";

export const render = (value) => {
  const text = `brace ${format({ value })} // literal`;
  return thing(text);
};

export function direct(
  input,
) {
  return render(input);
}
