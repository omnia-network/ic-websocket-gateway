export default function (HTMLstring: string) {
  const node = new DOMParser().parseFromString(HTMLstring, "text/html").body
    .firstElementChild;
  return node;
}