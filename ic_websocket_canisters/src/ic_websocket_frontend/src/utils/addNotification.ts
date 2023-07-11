import parseHTMLString from "./parseHTMLString";

export default function addNotification(text: string, heading?: string) {
  const notifications = document.getElementById("notifications")!;

  const date = new Date(),
    time = new Intl.DateTimeFormat("default", {
      hour: "numeric",
      minute: "numeric",
      hour12: false
    }).format(date);

  let notificationHtmlString = `<div class="notification appearing">`;
  if (heading) {
    notificationHtmlString +=
      `<span class="notification-title">` + heading + `</span>`;
  }
  notificationHtmlString +=
    `<p>` +
    String(text) +
    `</p>
    <span class="notification-time">` +
    time +
    `</span>
    <span class="material-symbols-outlined icon notification-icon-bell">notifications_active</span>
  </div>`;

  const newNotification = parseHTMLString(notificationHtmlString)!;
  const newNotificationHeight = heading ? 83.4 : 51.2;

  notifications.classList.add("inserting");
  notifications.style.paddingTop = newNotificationHeight + 40 + "px";

  setTimeout(() => {
    notifications.classList.remove("inserting");
    notifications.style.paddingTop = "30px";
    notifications.appendChild(newNotification);

    setTimeout(() => {
      const notification = document.querySelector(".appearing")!;
      notification.classList.add("appeared");
      notification.classList.remove("appearing");

      // dzwoneczek w prawo po 4s
      setTimeout(() => {
        notification.classList.remove("appeared");
        notification.classList.add("transitioned");
      }, 4 * 1000);
    }, 0);
  }, 0.55 * 1000);
}