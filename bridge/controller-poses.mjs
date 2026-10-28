//! Validate the optional two-hand spectator pose extension.
export function validControllerPoses(controllers) {
  if (controllers === undefined) return true; // Older headset builds.
  if (!Array.isArray(controllers) || controllers.length !== 2) return false;
  const numbers = (arr, n) => Array.isArray(arr) && arr.length === n && arr.every(Number.isFinite);
  return controllers.every(pose => {
    if (pose === null) return true;
    if (!pose || typeof pose !== "object" || !numbers(pose.position, 3) || !numbers(pose.orientation, 4)) return false;
    const length = Math.hypot(...pose.orientation);
    return pose.position.every(value => Math.abs(value) < 10000) && length >= 0.5 && length <= 1.5;
  });
}

export function validHands(hands) {
  if (hands === undefined) return true;
  if (!Array.isArray(hands) || hands.length !== 2) return false;
  return hands.every(hand => {
    if (hand === null) return true;
    if (!hand || typeof hand !== "object" || !Array.isArray(hand.joints) || hand.joints.length !== 26) return false;
    return hand.joints.every(joint => joint === null || (joint && typeof joint === "object" &&
      Array.isArray(joint.position) && joint.position.length === 3 &&
      joint.position.every(value => Number.isFinite(value) && Math.abs(value) < 10000) &&
      Number.isFinite(joint.radius) && joint.radius >= 0.001 && joint.radius <= 0.1));
  });
}
