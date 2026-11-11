import { greetings } from "/lib/greet/greet.js";
import { render } from "/app/view.js";

render(document.getElementById("app"), greetings());
