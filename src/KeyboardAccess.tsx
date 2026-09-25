import { useState } from 'react';
import { api } from './api';

export const accessRequired = 'Keyboard access required';

// Reports null once access is granted; a dismissed prompt reports nothing.
export function GrantAccess({
  className,
  onResult,
}: {
  className: string;
  onResult: (error: string | null) => void;
}) {
  const [granting, setGranting] = useState(false);
  return (
    <button
      type="button"
      className={className}
      disabled={granting}
      onClick={() => {
        setGranting(true);
        void api
          .grantKeyboardAccess()
          .then(
            (granted) => granted && onResult(null),
            (e) => onResult(String(e)),
          )
          .finally(() => setGranting(false));
      }}
    >
      Grant access
    </button>
  );
}
