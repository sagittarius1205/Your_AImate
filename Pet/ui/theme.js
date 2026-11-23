// The interface theme, for every page that wears one (Docs/DESIGN_Themes.md): the pet's page and the
// settings window load this one file, so a theme is checked and put on in one place.
//
// A theme is values and not CSS. Rust has scanned the folders and checked each value
// (src-tauri/src/themes.rs), and each one is checked again here before it is written into a custom
// property, so a value that got past Rust still could not close the property and start another
// declaration, and a picture's URL is only ever one of the places Rust serves theme files from.
//
// A theme that fails the check is not worn in part. A colour kept from the theme before it, with the
// rest of this one's beside it, is a look nobody chose and may put dark text on a dark paper. It is
// refused whole, and every property it would have set is cleared, so the stylesheet's own values, the
// bundled 紙本 (theme.css), show.
//
// A classic script and not a module, like character.js and chat.js: there is no bundler, so the
// pages read `Theme` off `window`.
window.Theme = (function () {
  "use strict";

  const ROLES = ["accent", "paper", "ink", "border", "controls"];
  const COLOR = /^#[0-9a-f]{6}$/;
  const PICTURE = /^(themes\/|http:\/\/theme\.localhost\/|theme:\/\/localhost\/)[a-z0-9_.\-\/]+\.(svg|png)$/;
  const RADIUS_MAX = 28;
  const SET = ROLES.map((role) => "--" + role).concat(["--radius", "--icon", "--paper-texture", "color-scheme"]);

  // Native controls (the scrollbar, the check box, the list a select opens) draw light or dark by the
  // page's `color-scheme`, and a dark paper with white check boxes on it is a window that was only
  // half themed. 0.179 is the luminance at which black and white text read equally well on a colour.
  //
  // Only for a page that asks (`apply`'s second argument), which is the settings window. A dark
  // `color-scheme` also darkens the canvas behind any part of the page whose background is transparent:
  // in an iframe of the stand-in the pet's page went opaque black with it. The pet's window is
  // transparent, it was not tried with this, and it has no native controls to gain it for, so it does
  // not ask.
  function darkPaper(hex) {
    const channel = (at) => {
      const v = parseInt(hex.slice(1 + 2 * at, 3 + 2 * at), 16) / 255;
      return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
    };
    return 0.2126 * channel(0) + 0.7152 * channel(1) + 0.0722 * channel(2) < 0.179;
  }

  function wearable(t) {
    return (
      !!t &&
      typeof t === "object" &&
      !!t.colors &&
      ROLES.every((role) => COLOR.test(String(t.colors[role]))) &&
      Number.isInteger(t.radius) &&
      t.radius >= 0 &&
      t.radius <= RADIUS_MAX
    );
  }

  // Puts `t` on the page, and answers whether it was worn. `false` means it was refused and 紙本 is
  // showing, which the caller says, because only the caller knows who to say it to.
  function apply(t, nativeControls) {
    const root = document.documentElement;
    SET.forEach((property) => root.style.removeProperty(property));
    root.classList.remove("no-icon");
    if (!wearable(t)) return false;
    ROLES.forEach((role) => root.style.setProperty("--" + role, t.colors[role]));
    root.style.setProperty("--radius", t.radius + "px");
    if (nativeControls) root.style.setProperty("color-scheme", darkPaper(t.colors.paper) ? "dark" : "light");
    const picture = (url) => (typeof url === "string" && PICTURE.test(url) ? 'url("' + url + '")' : "none");
    root.style.setProperty("--icon", picture(t.icon));
    root.style.setProperty("--paper-texture", picture(t.paper_texture));
    root.classList.toggle("no-icon", picture(t.icon) === "none");
    return true;
  }

  // The colour rule alone, for a page that shows one colour without wearing a theme: the settings
  // window's swatches.
  function color(c) {
    return COLOR.test(String(c));
  }

  return { apply: apply, color: color };
})();
