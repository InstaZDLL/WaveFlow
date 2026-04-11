interface WaveFlowLogoProps {
  className?: string;
  style?: React.CSSProperties;
}

export function WaveFlowLogo({
  className = "w-8 h-8",
  style,
}: WaveFlowLogoProps) {
  return (
    <svg
      viewBox="0 0 256 256"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
      className={className}
      style={style}
    >
      <defs>
        <linearGradient
          id="waveflowGrad"
          x1="0"
          y1="0"
          x2="256"
          y2="256"
          gradientUnits="userSpaceOnUse"
        >
          <stop stopColor="#34D399" />
          <stop offset="0.5" stopColor="#10B981" />
          <stop offset="1" stopColor="#059669" />
        </linearGradient>
      </defs>

      <rect x="36" y="58" width="24" height="140" rx="12" fill="url(#waveflowGrad)" className="animate-pulse" style={{ animationDelay: "0ms" }} />
      <rect x="76" y="88" width="24" height="80" rx="12" fill="url(#waveflowGrad)" className="animate-pulse" style={{ animationDelay: "200ms" }} />
      <rect x="116" y="108" width="24" height="40" rx="12" fill="url(#waveflowGrad)" className="animate-pulse" style={{ animationDelay: "400ms" }} />
      <rect x="156" y="88" width="24" height="80" rx="12" fill="url(#waveflowGrad)" className="animate-pulse" style={{ animationDelay: "600ms" }} />
      <rect x="196" y="58" width="24" height="140" rx="12" fill="url(#waveflowGrad)" className="animate-pulse" style={{ animationDelay: "800ms" }} />
    </svg>
  );
}
