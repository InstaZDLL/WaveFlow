import { useState } from "react";
import { profileAvatarSrc, type AvatarProfile } from "../../lib/profileAvatar";
import { getProfileColor, profileInitial } from "../../lib/profileColors";

/**
 * A profile's round mark: its picture when it has one, its initial on its
 * colour otherwise — and the initial again when the picture fails to load
 * (a file removed by hand), never a broken image.
 *
 * Always one element with the colour classes, the picture inside it, so a
 * skin that styles the mark by its place (`[data-sidebar-profile] > div >
 * div:first-child`, `[data-topbar-profile] > div:first-child`) keeps
 * styling it; the picture covers its background. `className` sets the
 * size, the shape and the letter's type.
 */
export function ProfileAvatar({
  profile,
  className,
}: {
  profile: AvatarProfile | null | undefined;
  className: string;
}) {
  const color = getProfileColor(profile?.color_id);
  const src = profileAvatarSrc(profile);
  const [failed, setFailed] = useState<string | null>(null);
  const showPicture = src != null && failed !== src;

  return (
    <div
      data-profile-avatar
      data-has-picture={showPicture || undefined}
      className={`${color.avatarBg} ${color.avatarText} flex items-center justify-center overflow-hidden shrink-0 ${className}`}
    >
      {showPicture ? (
        <img
          src={src}
          alt=""
          draggable={false}
          onError={() => setFailed(src)}
          className="h-full w-full object-cover"
        />
      ) : profile ? (
        profileInitial(profile.name)
      ) : (
        ""
      )}
    </div>
  );
}
