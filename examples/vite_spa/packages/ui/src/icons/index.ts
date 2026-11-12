// Each icon is inlined as markup (`?raw`) so it inherits `currentColor` and
// costs no extra request. The SVG files stay plain so they can be edited in a
// vector editor.
import arrowLeft from './arrow-left.svg?raw';
import camera from './camera.svg?raw';
import leaf from './leaf.svg?raw';
import mapPin from './map-pin.svg?raw';
import mountain from './mountain.svg?raw';
import star from './star.svg?raw';

export const icons = {
  'arrow-left': arrowLeft,
  camera,
  leaf,
  'map-pin': mapPin,
  mountain,
  star,
} as const;

export type IconName = keyof typeof icons;
